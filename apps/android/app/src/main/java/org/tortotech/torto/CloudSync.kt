package org.tortotech.torto

import android.content.Context
import androidx.work.*
import java.io.File
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.*
import kotlin.coroutines.resume
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import org.tortotech.torto.core.*

internal object NativeRuntime {
    @Volatile private var instance:MobileCore?=null
    @Synchronized fun core(context:Context):MobileCore=instance ?: MobileCore(File(context.applicationContext.filesDir,"core").absolutePath).also {instance=it}
}
data class CloudStatus(val running:Boolean=false,val stage:String="",val completed:UInt=0u,val total:UInt=0u,val lastSuccess:Long=0,val error:String?=null,val revision:Long=0,val booksCompleted:UInt=0u,val booksTotal:UInt=0u,val retryAt:Long=0,val coverRevision:Long=0)
/** One transaction per process. Native also serializes library manifest mutations. */
internal object CloudRuntime {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val mutex = Mutex()
    private val wake = kotlinx.coroutines.channels.Channel<Unit>(kotlinx.coroutines.channels.Channel.CONFLATED)
    private val mutable = MutableStateFlow(CloudStatus())
    val state = mutable.asStateFlow()
    @Volatile var foreground = false; private set
    private var timer: Job? = null
    @Volatile private var request: ChatRequest? = null
    private val generation = java.util.concurrent.atomic.AtomicLong()
    const val PERIODIC = "torto-cloud-periodic"
    const val EXIT = "torto-cloud-exit"
    private const val RETRY = "torto-cloud-retry"

    fun cancel() {
        generation.incrementAndGet()
        runCatching { request?.cancel() }
        timer?.cancel(); timer = null
        mutable.update { it.copy(running = false, revision = it.revision + 1) }
    }
    // Rust queued the library change in the same transaction as the import/remove.
    fun libraryChanged() { mutable.update { it.copy(revision = it.revision + 1) }; wake.trySend(Unit) }
    fun initialize(context: Context) { scope.launch { runCatching {
        val last = context.getSharedPreferences("cloud-status", 0).getLong("success", 0)
        if (mutable.value.lastSuccess == 0L) mutable.value = mutable.value.copy(lastSuccess = last)
        schedule(context)
    } } }
    suspend fun schedule(context: Context) {
        val options = NativeRuntime.core(context).cloudOptions()
        val manager = WorkManager.getInstance(context)
        if (options.enabled && options.background) {
            val work = PeriodicWorkRequestBuilder<CloudWorker>(15, TimeUnit.MINUTES)
                .setInitialDelay(15, TimeUnit.MINUTES)
                .setConstraints(networkConstraint()).build()
            manager.enqueueUniquePeriodicWork(PERIODIC, ExistingPeriodicWorkPolicy.KEEP, work)
        } else {
            manager.cancelUniqueWork(PERIODIC)
            manager.cancelUniqueWork(EXIT)
            manager.cancelUniqueWork(RETRY)
        }
    }
    fun foreground(context: Context, value: Boolean) {
        foreground = value
        timer?.cancel(); timer = null
        if (!value) return
        val manager = WorkManager.getInstance(context)
        manager.cancelUniqueWork(EXIT)
        manager.cancelUniqueWork(RETRY)
        timer = scope.launch {
            val options = runCatching { NativeRuntime.core(context).cloudOptions() }.getOrNull()
                ?: return@launch
            if (!options.enabled || !options.foreground) return@launch
            while (isActive && foreground) {
                val run = runCatching { perform(context, CloudSyncTrigger.AUTOMATIC, options = options) }.getOrNull()
                    ?: break
                if (run.nextPollMs == 0uL) break
                withTimeoutOrNull(run.nextPollMs.toLong().coerceAtLeast(1)) { wake.receive() }
            }
        }
    }
    suspend fun exit(context: Context) {
        if (foreground) return
        val options = NativeRuntime.core(context).cloudOptions()
        if (!options.enabled || !options.background) return
        val work = OneTimeWorkRequestBuilder<CloudWorker>().setInitialDelay(2, TimeUnit.SECONDS)
            .setInputData(workDataOf("flushReading" to true))
            .setConstraints(networkConstraint()).build()
        WorkManager.getInstance(context).enqueueUniqueWork(EXIT, ExistingWorkPolicy.KEEP, work)
    }
    private fun networkConstraint() = Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build()
    private suspend fun retry(context: Context, delay: ULong?) = withContext(Dispatchers.IO) {
        val manager = WorkManager.getInstance(context)
        if (delay == null) return@withContext
        if (manager.getWorkInfosForUniqueWork(RETRY).get().any {
            it.state == WorkInfo.State.ENQUEUED || it.state == WorkInfo.State.BLOCKED
        }) return@withContext
        val work = OneTimeWorkRequestBuilder<CloudWorker>()
            .setInitialDelay(delay.toLong().coerceAtLeast(1), TimeUnit.MILLISECONDS)
            .setConstraints(networkConstraint()).build()
        manager.enqueueUniqueWork(RETRY, ExistingWorkPolicy.APPEND_OR_REPLACE, work)
    }
    fun launchWorker(context: Context, worker: CloudWorker, done: (Throwable?) -> Unit) {
        scope.launch {
            try {
                perform(context, if (worker.inputData.getBoolean("flushReading", false)) CloudSyncTrigger.FLUSH_READING else CloudSyncTrigger.AUTOMATIC, worker)
                done(null)
            } catch (e: Throwable) { done(e) }
        }
    }
    suspend fun perform(
        context: Context,
        trigger: CloudSyncTrigger,
        worker: CloudWorker? = null,
        options: CloudOptions? = null,
    ): CloudSyncRun? = mutex.withLock {
        val token = generation.get()
        val core = NativeRuntime.core(context)
        val config = options ?: core.cloudOptions()
        if (!config.enabled || worker != null && !config.background) return@withLock null
        if (worker != null && foreground && config.foreground) return@withLock null
        val control = ChatRequest(); request = control; worker?.control = control
        if (worker?.isStopped == true) control.cancel()
        val changed = java.util.concurrent.atomic.AtomicBoolean()
        control.observeAi(object: AiProgressListener {
            override fun update(stage: String, completed: UInt, total: UInt) {
                if (stage == "library_changed" || stage == "cover_changed") changed.set(true)
                if (token != generation.get()) return
                mutable.update { status -> when (stage) {
                    "library_changed" -> status.copy(running = true, revision = status.revision + 1, booksCompleted = completed, booksTotal = total, error = null, retryAt = 0)
                    "cover_changed" -> status.copy(revision = status.revision + 1, coverRevision = status.coverRevision + 1)
                    "books" -> status.copy(running = true, booksCompleted = completed, booksTotal = total, stage = "downloading", error = null, retryAt = 0)
                    "preparing" -> status.copy(running = true, stage = stage, booksCompleted = 0u, booksTotal = 0u, completed = 0u, total = 0u, error = null, retryAt = 0)
                    else -> status.copy(running = true, stage = stage, completed = completed, total = total, error = null, retryAt = 0)
                } }

            }
        })
        try {
            // Keep the transaction owned until a cancelled native worker unwinds.
            val run = withContext(NonCancellable) { core.syncScheduled(config.account, trigger, control) }
            if (token != generation.get()) return@withLock null
            if (run.summary != null) {
                val now = System.currentTimeMillis()
                context.getSharedPreferences("cloud-status", 0).edit().putLong("success", now).apply()
                mutable.update { it.copy(running = false, lastSuccess = now, error = run.error, revision = it.revision + 1, retryAt = run.retryAfterMs?.let { ms -> now + ms.toLong() } ?: 0) }
            } else {
                mutable.update { it.copy(running = false, error = run.error ?: it.error, retryAt = run.retryAfterMs?.let { ms -> System.currentTimeMillis() + ms.toLong() } ?: 0) }
            }
            if (!foreground && config.background) retry(context, run.retryAfterMs)
            run
        } catch (e: Exception) {
            if (token == generation.get()) mutable.value = mutable.value.copy(running = false, error = if (e is CancellationException) null else e.message?.take(500))
            throw e
        } finally {
            if (changed.get()) mutable.update { it.copy(revision = it.revision + 1) }
            request = null; worker?.control = null; control.close()
        }
    }
}
@androidx.annotation.Keep
class CloudWorker(context: Context, parameters: WorkerParameters): CoroutineWorker(context, parameters) {
    @Volatile internal var control: ChatRequest? = null
    override suspend fun doWork(): Result = suspendCancellableCoroutine { continuation ->
        continuation.invokeOnCancellation { runCatching { control?.cancel() } }
        CloudRuntime.launchWorker(applicationContext, this) { error ->
            // Domain errors already carry a Rust-computed retry wake. Unexpected
            // platform/bridge failures do not invent a second backoff policy.
            if (continuation.isActive) continuation.resume(if (error == null) Result.success() else Result.failure())
        }
    }
}

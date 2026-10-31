package org.tortotech.torto

import android.content.Context
import androidx.work.*
import java.io.File
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.*
import kotlin.coroutines.resume
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import org.tortotech.torto.core.*

internal object NativeRuntime {
    @Volatile private var instance:MobileCore?=null
    @Synchronized fun core(context:Context):MobileCore=instance ?: MobileCore(File(context.applicationContext.filesDir,"core").absolutePath).also {instance=it}
}
data class CloudStatus(val running:Boolean=false,val stage:String="",val completed:UInt=0u,val total:UInt=0u,val lastSuccess:Long=0,val error:String?=null,val revision:Long=0)
/** One transaction per process. Native also serializes library manifest mutations. */
internal object CloudRuntime {
    private val scope=CoroutineScope(SupervisorJob()+Dispatchers.IO)
    private val mutex=Mutex()
    private val mutable=MutableStateFlow(CloudStatus())
    val state=mutable.asStateFlow()
    @Volatile var foreground=false;private set
    private var timer:Job?=null
    @Volatile private var request:ChatRequest?=null
    private val generation=java.util.concurrent.atomic.AtomicLong()
    private val libraryRevision=java.util.concurrent.atomic.AtomicLong()
    private var libraryJob:Job?=null
    @Volatile private var lastAccount=""
    @Volatile private var lastFull=0L
    const val PERIODIC="torto-cloud-periodic";const val EXIT="torto-cloud-exit"
    fun cancel(){generation.incrementAndGet();runCatching {request?.cancel()};lastFull=0;lastAccount="";mutable.value=mutable.value.copy(running=false)}
    fun libraryChanged(context:Context){
        libraryRevision.incrementAndGet();lastFull=0;libraryJob?.cancel()
        libraryJob=scope.launch {delay(1000);runCatching {val o=NativeRuntime.core(context).cloudOptions();if(foreground&&o.enabled&&o.foreground)perform(context,true)}}
    }
    fun initialize(context:Context){scope.launch {runCatching {
        val last=context.getSharedPreferences("cloud-status",0).getLong("success",0)
        if(mutable.value.lastSuccess==0L)mutable.value=mutable.value.copy(lastSuccess=last)
        schedule(context)
    }}}
    suspend fun schedule(context:Context){
        val o=NativeRuntime.core(context).cloudOptions();val manager=WorkManager.getInstance(context)
        if(o.enabled&&o.background){
            val work=PeriodicWorkRequestBuilder<CloudWorker>(15,TimeUnit.MINUTES).setInitialDelay(15,TimeUnit.MINUTES).setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build()).setBackoffCriteria(BackoffPolicy.EXPONENTIAL,30,TimeUnit.SECONDS).build()
            manager.enqueueUniquePeriodicWork(PERIODIC,ExistingPeriodicWorkPolicy.KEEP,work)
        }else{manager.cancelUniqueWork(PERIODIC);manager.cancelUniqueWork(EXIT)}
    }
    fun foreground(context:Context,value:Boolean){
        foreground=value;timer?.cancel();timer=null
        if(!value)return
        WorkManager.getInstance(context).cancelUniqueWork(EXIT)
        timer=scope.launch {
            while(isActive&&foreground){
                val o=runCatching {NativeRuntime.core(context).cloudOptions()}.getOrNull()
                if(o?.enabled!=true||!o.foreground)break
                runCatching {perform(context,false)}
                delay(45_000)
            }
        }
    }
    suspend fun exit(context:Context){
        if(foreground)return
        val o=NativeRuntime.core(context).cloudOptions();if(!o.enabled||!o.background)return
        val work=OneTimeWorkRequestBuilder<CloudWorker>().setInitialDelay(2,TimeUnit.SECONDS).setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build()).setBackoffCriteria(BackoffPolicy.EXPONENTIAL,30,TimeUnit.SECONDS).build()
        WorkManager.getInstance(context).enqueueUniqueWork(EXIT,ExistingWorkPolicy.KEEP,work)
    }
    fun launchWorker(context:Context,worker:CloudWorker,done:(Throwable?)->Unit){scope.launch {try{perform(context,false,worker);done(null)}catch(e:Throwable){done(e)}}}
    suspend fun perform(context:Context,force:Boolean,worker:CloudWorker?=null):SyncSummary?=mutex.withLock {
        val token=generation.get();val revision=libraryRevision.get();val core=NativeRuntime.core(context);val options=core.cloudOptions()
        if(!options.enabled)return@withLock null
        if(worker!=null&&!options.background)return@withLock null
        val full=force||options.account!=lastAccount||System.currentTimeMillis()-lastFull>10*60_000
        val control=ChatRequest();request=control;worker?.control=control;if(worker?.isStopped==true)control.cancel()
        control.observeAi(object:AiProgressListener {override fun update(stage:String,completed:UInt,total:UInt){if(token!=generation.get())return;mutable.value=mutable.value.copy(running=true,stage=stage,completed=completed,total=total)}})
        mutable.value=mutable.value.copy(running=true,error=null,stage="preparing",completed=0u,total=0u)
        try {
            // Cancellation must not release the transaction lock while a native worker is still running.
            val report=withContext(NonCancellable){core.syncCancellable(options.account,!full,control)}
            if(token!=generation.get())return@withLock null
            if(full){lastFull=if(revision==libraryRevision.get())System.currentTimeMillis() else 0;lastAccount=options.account}
            context.getSharedPreferences("cloud-status",0).edit().putLong("success",System.currentTimeMillis()).apply()
            mutable.value=mutable.value.copy(running=false,lastSuccess=System.currentTimeMillis(),error=report.warning,revision=mutable.value.revision+1)
            report
        }catch(e:Exception){if(token==generation.get())mutable.value=mutable.value.copy(running=false,error=if(e is CancellationException) null else e.message?.take(500));throw e}
        finally{request=null;worker?.control=null;control.close()}
    }
}
@androidx.annotation.Keep
class CloudWorker(context:Context,parameters:WorkerParameters):CoroutineWorker(context,parameters){
    @Volatile internal var control:ChatRequest?=null
    override suspend fun doWork():Result=suspendCancellableCoroutine {continuation->
        continuation.invokeOnCancellation {runCatching {control?.cancel()}}
        CloudRuntime.launchWorker(applicationContext,this){error->
            if(continuation.isActive)continuation.resume(if(error==null)Result.success() else if(runAttemptCount<3)Result.retry() else Result.failure())
        }
    }
}

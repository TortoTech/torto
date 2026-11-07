package org.tortotech.torto

import android.content.Context
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import org.tortotech.torto.core.ReadingFontDownload

internal data class ReadingFontState(
    val running:Boolean=false, val ready:Boolean=false, val revision:Long=0,
    val downloaded:UInt=0u, val error:String?=null,
)

/** A process-lived job survives activity recreation; automatic starts and manual
 * taps share one batch. Network/cache policy stays in the shared Rust service. */
internal class ReadingFontDownloads(private val scope:CoroutineScope) {
    private val mutable=MutableStateFlow(ReadingFontState())
    val state=mutable.asStateFlow()
    private var started=false
    @Synchronized fun request(automatic:Boolean,load:suspend ()->ReadingFontDownload) {
        if(automatic&&started)return
        started=true
        if(mutable.value.running)return
        mutable.update {it.copy(running=true,error=null)}
        scope.launch {
            try {
                val result=load()
                mutable.update {it.copy(running=false,ready=result.ready,downloaded=result.downloaded,
                    revision=it.revision+1,error=result.error)}
            } catch(e:CancellationException) {
                mutable.update {it.copy(running=false)}
                throw e
            } catch(e:Exception) {
                mutable.update {it.copy(running=false,downloaded=0u,revision=it.revision+1,error=e.message?:"Font download failed")}
            }
        }
    }
}

internal object ReadingFonts {
    private val downloads=ReadingFontDownloads(CoroutineScope(SupervisorJob()+Dispatchers.IO))
    val state=downloads.state
    // Instrumentation supplies an offline loader before launching an activity.
    internal var loader:suspend (Context)->ReadingFontDownload={NativeRuntime.core(it).downloadReadingFonts()}
    fun start(context:Context,automatic:Boolean=true) {
        val application=context.applicationContext
        downloads.request(automatic){loader(application)}
    }
}

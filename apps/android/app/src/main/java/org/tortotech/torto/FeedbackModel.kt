package org.tortotech.torto

import android.app.Application
import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.net.Uri
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import org.tortotech.torto.core.*
import java.io.File

internal data class FeedbackUiState(
    val visible:Boolean=false,val loading:Boolean=false,val working:Boolean=false,val sending:Boolean=false,
    val kind:String="bug",val description:String="",val count:UInt=0u,val valid:Boolean=false,val locked:Boolean=false,
    val images:List<FeedbackImage> = emptyList(),val context:String="",val status:String="editing",val error:String?=null,
    val retryUntil:ULong=0u,val issueUrl:String?=null,val issueNumber:ULong?=null,val preview:Bitmap?=null,
)
/** UI-only adapter. Rust persists drafts, owns bounded attachments and submits
 * exactly the same frozen report and request ID as desktop. */
internal class FeedbackModel(application:Application):AndroidViewModel(application){
    private val mutable=MutableStateFlow(FeedbackUiState())
    val state=mutable.asStateFlow()
    private val operations=Mutex()
    private var session:MobileFeedback?=null
    private var generation=0L
    private var previewGeneration=0L
    private var editJob:Job?=null
    fun open(reader:ReaderModel,environment:FeedbackEnvironment){
        if(state.value.visible)return
        if(session!=null&&(state.value.locked||state.value.sending)&&state.value.status!="submitted"){
            mutable.update {it.copy(visible=true)};return
        }
        val token=++generation
        mutable.value=FeedbackUiState(visible=true,loading=true)
        viewModelScope.launch {operations.withLock {
            try {
                val created=reader.feedbackSession(environment)
                if(token!=generation||!state.value.visible){created.close();return@withLock}
                val snapshot=created.snapshot()
                if(token!=generation||!state.value.visible){created.close();return@withLock}
                session?.close();session=created
                apply(snapshot,restore=true)
            }catch(e:CancellationException){throw e}catch(_:Exception){if(token==generation&&state.value.visible)mutable.update {it.copy(loading=false,error="storage")}}
        }}
    }
    private fun apply(result:FeedbackState,restore:Boolean=false){
        mutable.update {old->if(old.loading&&!restore)return@update old
            old.copy(loading=false,
            kind=if(restore||result.locked)result.kind else old.kind,description=if(restore||result.locked)result.description else old.description,
            count=result.count,valid=result.valid&&(restore||result.locked||old.kind==result.kind&&old.description==result.description),locked=result.locked,
            images=if(!restore&&old.images.size==result.images.size)old.images else result.images,
            context=result.context,status=result.status,error=result.error,retryUntil=result.retryUntil,
            issueUrl=result.issueUrl,issueNumber=result.issueNumber)
        }
    }
    fun edit(kind:String=state.value.kind,description:String=state.value.description){
        if(state.value.loading||state.value.locked||state.value.working||state.value.sending)return
        mutable.update {it.copy(kind=kind,description=description,valid=false,error=null)}
        editJob?.cancel()
        editJob=viewModelScope.launch {delay(600);operations.withLock {flush()}}
    }
    private suspend fun flush(){
        val current=session ?: return
        val draft=state.value
        if(draft.loading||draft.locked||draft.status=="submitted")return
        try {withContext(NonCancellable){val result=current.edit(draft.kind,draft.description);if(session===current)apply(result)}}
        catch(e:CancellationException){throw e}catch(_:Exception){mutable.update {it.copy(error="storage")}}
    }
    fun close(){
        generation++;previewGeneration++
        editJob?.cancel()
        mutable.update {it.copy(visible=false,preview=null)}
        viewModelScope.launch {operations.withLock {flush()}}
    }
    fun submit(){
        if(state.value.working||state.value.sending||state.value.loading)return
        editJob?.cancel();mutable.update {it.copy(sending=true,error=null)}
        viewModelScope.launch {
            val current=operations.withLock {flush();session} ?: run {mutable.update {it.copy(sending=false,error="storage")};return@launch}
            try {val result=current.submit();if(session===current)apply(result)}
            catch(e:CancellationException){throw e}catch(_:Exception){mutable.update {it.copy(error="network")}}
            finally {if(session===current)mutable.update {it.copy(sending=false)}}
        }
    }
    fun addImages(context:Context,uris:List<Uri>){
        if(uris.isEmpty()||state.value.working||state.value.sending||state.value.locked)return
        if(uris.size>3-state.value.images.size){mutable.update {it.copy(error="limit")};return}
        editJob?.cancel();mutable.update {it.copy(working=true,error=null)}
        viewModelScope.launch {operations.withLock {
            try {
                flush();val current=session ?: return@withLock
                for(uri in uris){
                    val file=withContext(Dispatchers.IO){
                        val tmp=File.createTempFile("feedback-",".image",context.cacheDir)
                        try {context.contentResolver.openInputStream(uri).use {input->requireNotNull(input);tmp.outputStream().use {output->
                            val bytes=ByteArray(65536);var size=0
                            while(true){val n=input.read(bytes);if(n<0)break;size+=n;require(size<=20*1024*1024);output.write(bytes,0,n)}
                        }};tmp}catch(e:Exception){tmp.delete();throw e}
                    }
                    try {withContext(NonCancellable){apply(current.addImage(file.absolutePath))}}finally{withContext(Dispatchers.IO){file.delete()}}
                }
            }catch(e:CancellationException){throw e}catch(_:Exception){mutable.update {it.copy(error="image")}}
            finally {mutable.update {it.copy(working=false)}}
        }}
    }
    fun removeImage(index:UInt){
        if(state.value.working||state.value.sending||state.value.locked)return
        mutable.update {it.copy(working=true,error=null)}
        viewModelScope.launch {operations.withLock {
            try {session?.let {apply(it.removeImage(index))}}catch(e:CancellationException){throw e}catch(_:Exception){mutable.update {it.copy(error="image")}}
            finally{mutable.update {it.copy(working=false)}}
        }}
    }
    fun preview(index:UInt){
        if(state.value.loading||state.value.working)return
        val current=session ?: return
        val token=++previewGeneration
        viewModelScope.launch {
            try {val bitmap=withContext(Dispatchers.IO){val bytes=current.image(index);BitmapFactory.decodeByteArray(bytes,0,bytes.size)}
                if(token==previewGeneration&&session===current&&state.value.visible&&!state.value.loading)mutable.update {it.copy(preview=bitmap)}
            }catch(e:CancellationException){throw e}catch(_:Exception){mutable.update {it.copy(error="image")}}
        }
    }
    fun closePreview(){previewGeneration++;mutable.update {it.copy(preview=null)}}
}

package org.tortotech.torto

import android.app.Application
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.util.LruCache
import android.net.Uri
import android.provider.OpenableColumns
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import java.io.File
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import org.tortotech.torto.core.*
import org.tortotech.torto.core.Annotation

data class ReaderInteraction(val kind:String,val text:String,val target:String,val frame:ReaderFrame?=null,val markup:String="")
data class ReaderFrame(val bitmap: Bitmap, val info: PageInfo)
data class ScreenState(
    val books: List<BookSummary> = emptyList(), val loaded: Boolean = false, val active: BookSummary? = null,
    val frame: ReaderFrame? = null, val toc: List<TocItem> = emptyList(),
    val hits: List<SearchHit> = emptyList(), val selected: SelectedText? = null,
    val busy: Boolean = false, val error: String? = null, val notice: String? = null,
    val services: ServiceSettings? = null,
    val cloud:CloudOptions?=null,val cloudPresets:List<CloudPreset> = emptyList(),val cloudStatus:CloudStatus=CloudStatus(),
    val statsPeriod:StatisticsPeriod?=null,val statsKind:String="week",val statsOffset:Int=0,val statsLoading:Boolean=false,val bookStatistics:BookStatistics?=null,val statsBookId:String?=null,
    val appVersion:String="",val updateSource:String="automatic",val checkingUpdate:Boolean=false,val appUpdate:AppUpdate?=null,val updateChecked:Boolean=false,val downloadingUpdate:Boolean=false,val updateProgress:UInt=0u,val updateError:String?=null,val downloadedUpdate:String?=null,val diagnosticFile:String?=null,
    val monoTypeface:android.graphics.Typeface=android.graphics.Typeface.MONOSPACE,
    val reading:ReadingOptions?=null,val fontFamilies:FontFamilies?=null,val aiOptions:AiOptions?=null,
    val providers:List<Provider> = emptyList(),val providerKinds:List<ProviderKind> = emptyList(),
    val modelChoices:Map<String,List<String>> = emptyMap(),val modelFetchError:String?=null,
    val settingsRevision:Long=0, val aiStage:String="",val aiCompleted:UInt=0u,val aiTotal:UInt=0u,val aiError:String?=null,val aiPaused:Boolean=false,
    val annotations: List<Annotation> = emptyList(), val statistics: ReadingStatistics? = null,
    val messages: List<ChatMessage> = emptyList(), val chatWorking: Boolean = false,
    val chatDraft:String="",val draftImages:List<ChatAttachment> = emptyList(),val sendingImages:List<ChatAttachment> = emptyList(),
    val chatContent: String = "", val chatReasoning: String = "", val chatActivity: String = "", val chatQuestion: String = "",
    val changes: List<ChatChange> = emptyList(), val translated: Boolean = false,
    val selectionMode: String = "word",
    val interaction:ReaderInteraction?=null,val focus:Boolean=false,val chromeVisible:Boolean=true,
    val processing:Boolean=false,val autoLayout:Boolean=false,
    val appTheme: String = "system", val language: String = "system",
    val fontSize: Float = 20f, val unified: Boolean = false, val dark: Boolean = false,
)
object NativePixels {
    init { System.loadLibrary("torto_android") }
    @JvmStatic external fun copyPage(id: Long, bitmap: Bitmap): Boolean
}
class ReaderModel(application: Application) : AndroidViewModel(application) {
    private val core = NativeRuntime.core(application)
    private val mutable = MutableStateFlow(ScreenState())
    val state = mutable.asStateFlow()
    private val operations = Mutex()
    private var statsGeneration=0L
    private var updateGeneration=0L
    private var updateRequest:ChatRequest?=null
    private var reader: MobileReader? = null
    private var readerBook: String? = null
    @Volatile private var generation = 0L
    private var viewport: Viewport? = null
    private var resizeJob: Job? = null
    private var chatRequest: ChatRequest? = null
    private var chatJob: Job? = null
    private var aiRequest:ChatRequest?=null
    private val processedPages=linkedSetOf<String>()
    private var prefetchKey:String?=null
    private var scrollPending=0f
    private var scrollJob:Job?=null
    private var selectionPending:FloatArray?=null
    private var selectionJob:Job?=null
    private var selectionHandle=false
    private val missingCovers = LruCache<String, Boolean>(256)
    private val covers = object : LruCache<String, Bitmap>(8 * 1024 * 1024) { override fun sizeOf(key: String, value: Bitmap) = value.allocationByteCount }
    suspend fun cover(id: String): Bitmap? {
        covers.get(id)?.let { return it }
        if (missingCovers.get(id) == true) return null
        return try {
            val bytes = core.cover(id) ?: return null.also { missingCovers.put(id,true) }
            withContext(Dispatchers.Default) { BitmapFactory.decodeByteArray(bytes,0,bytes.size)?.also { covers.put(id,it) } }
        } catch (e: CancellationException) { throw e } catch (_: Exception) { missingCovers.put(id,true); null }
    }
    init {
        viewModelScope.launch {CloudRuntime.state.collect {status->
            val changed=status.revision!=state.value.cloudStatus.revision
            mutable.update {it.copy(cloudStatus=status)}
            if(changed)runOperation {val books=core.books();covers.evictAll();missingCovers.evictAll();mutable.update {it.copy(books=books)}}
        }}
        runOperation {
        val preferences=withContext(Dispatchers.IO) { application.getSharedPreferences("reading",0).let { Triple(it.getFloat("size",20f),it.getBoolean("unified",false),it.getBoolean("dark",false)) } }
        val system=withContext(Dispatchers.IO) { application.getSharedPreferences("interface",0).let { it.getString("theme","system")!! to it.getString("language","system")!! } }
        mutable.update { it.copy(appTheme=system.first,language=system.second) }
        val automatic=withContext(Dispatchers.IO){application.getSharedPreferences("reading",0).let {it.getBoolean("autoLayout",false) to it.getBoolean("focus",false)}}
        mutable.update {it.copy(autoLayout=automatic.first,focus=automatic.second)}
        val maintenance=withContext(Dispatchers.IO){application.packageManager.getPackageInfo(application.packageName,0).versionName.orEmpty() to application.getSharedPreferences("interface",0).getString("updateSource","automatic").orEmpty()}
        val cloud=core.cloudOptions();mutable.update {it.copy(cloud=cloud,appVersion=maintenance.first,updateSource=maintenance.second)}
        val reading=core.readingOptions();var ai=core.aiOptions()
        val migrated=withContext(Dispatchers.IO){application.getSharedPreferences("reading",0).getBoolean("aiSettingsMigrated",false)}
        if(!migrated){ai=core.saveAiOptions(ai.copy(layoutEnabled=automatic.first&&ai.layoutEnabled));withContext(Dispatchers.IO){application.getSharedPreferences("reading",0).edit().putBoolean("aiSettingsMigrated",true).commit()}}
        updateMonospace(reading.monospaceFont)
        mutable.update {it.copy(reading=reading,aiOptions=ai,autoLayout=ai.layoutEnabled,translated=reading.translated)}
        val books = core.books(); val services = runCatching { core.serviceSettings() }.getOrNull(); mutable.update { it.copy(books = books,services=services,loaded=true,fontSize=preferences.first,unified=preferences.second,dark=preferences.third) }
    } }

    private fun runOperation(block: suspend () -> Unit) {
        viewModelScope.launch {
            operations.withLock {
                mutable.update { it.copy(busy = true, error = null) }
                try { withContext(NonCancellable) { block() } }
                catch (e: CancellationException) { throw e }
                catch (e: Exception) { mutable.update { it.copy(error = e.message ?: "操作失败") } }
                finally { mutable.update { it.copy(busy = false) } }
            }
        }
    }
    fun clearError() { mutable.update { it.copy(error = null) } }
    fun clearNotice() { mutable.update { it.copy(notice = null) } }
    private suspend fun reloadSettings(){
        val providers=core.providers();val kinds=core.providerKinds();val ai=core.aiOptions();val reading=core.readingOptions();val services=core.serviceSettings();val cloud=core.cloudOptions();val presets=core.cloudPresets()
        mutable.update {it.copy(providers=providers,providerKinds=kinds,cloud=cloud,cloudPresets=presets,aiOptions=ai,reading=reading,autoLayout=ai.layoutEnabled,services=services)}
    }
    fun loadServices() = runOperation {reloadSettings()}
    fun loadFonts() = runOperation {val fonts=core.fontFamilies();mutable.update {it.copy(fontFamilies=fonts)}}
    private suspend fun updateMonospace(family:String){
        val file=core.monospaceFile(family)
        val face=withContext(Dispatchers.IO){runCatching {file?.let {android.graphics.Typeface.Builder(File(it.path)).setTtcIndex(it.index.toInt()).build()}}.getOrNull() ?: android.graphics.Typeface.MONOSPACE}
        mutable.update {it.copy(monoTypeface=face)}
    }
    fun saveReadingOptions(options:ReadingOptions){cancelAi();val token=generation;runOperation{
        core.saveReadingOptions(options)
        if(state.value.reading?.monospaceFont!=options.monospaceFont)updateMonospace(options.monospaceFont)
        if(token==generation){mutable.update {it.copy(reading=options)};reader?.let {publish(it.applyReadingOptions(options),token)}}
        processedPages.clear();prefetchKey=null
    }}
    fun saveProvider(provider:Provider,key:String,clearKey:Boolean,done:()->Unit={}) {cancelAi();runOperation {core.saveProvider(provider,key.takeIf {it.isNotBlank()},clearKey);reloadSettings();reader?.let {publish(it.reloadAiOptions())};processedPages.clear();prefetchKey=null;mutable.update {it.copy(settingsRevision=it.settingsRevision+1)};done()}}
    fun deleteProvider(id:String){cancelAi();runOperation {core.deleteProvider(id);reloadSettings();reader?.let {publish(it.reloadAiOptions())};processedPages.clear();prefetchKey=null;mutable.update {it.copy(settingsRevision=it.settingsRevision+1)}}}
    fun fetchModels(provider:Provider,key:String,clearKey:Boolean=false)=runOperation {
        val cacheKey=provider.kind+":"+provider.baseUrl
        mutable.update {it.copy(modelFetchError=null)}
        try{val models=core.fetchModels(provider,if(clearKey)"" else key.takeIf {it.isNotBlank()});mutable.update {it.copy(modelChoices=mapOf(cacheKey to models))}}
        catch(e:Exception){mutable.update {it.copy(modelFetchError=e.message)}}
    }
    fun saveAiOptions(options:AiOptions){cancelAi();val token=generation;runOperation {
        val saved=core.saveAiOptions(options);mutable.update {it.copy(aiOptions=saved,autoLayout=saved.layoutEnabled,aiError=null,aiPaused=false)}
        if(token==generation)reader?.let {publish(it.reloadAiOptions(),token)}
        processedPages.clear();prefetchKey=null
    }}
    fun saveAi(url: String, key: String, layout: String, translation: String, chat: String, language: String) = runOperation {
        core.saveAiSettings(url.trim(),key.takeIf { it.isNotBlank() },layout.trim(),translation.trim(),chat.trim(),language)
        val settings=core.serviceSettings();mutable.update { it.copy(services=settings,notice="AI 设置已保存") }
    }
    fun saveSync(url: String, user: String, password: String) = runOperation {
        core.saveSyncSettings(url.trim(),user.trim(),password.takeIf { it.isNotBlank() })
        val settings=core.serviceSettings();mutable.update { it.copy(services=settings,notice="同步设置已保存") }
    }
    fun saveCloud(options:CloudOptions,password:String,clearPassword:Boolean=false,done:()->Unit={}){
        CloudRuntime.cancel()
        runOperation {val saved=core.saveCloudOptions(options,if(clearPassword)"" else password.takeIf {it.isNotBlank()});val services=core.serviceSettings();mutable.update {it.copy(cloud=saved,services=services)};withContext(Dispatchers.IO){CloudRuntime.schedule(getApplication());if(CloudRuntime.foreground)CloudRuntime.foreground(getApplication(),true)};done()}
    }
    fun synchronize(){viewModelScope.launch {
        try{val report=CloudRuntime.perform(getApplication(),true);report?.let {mutable.update {it.copy(notice="同步完成：上传 ${report.uploaded} 本，下载 ${report.downloaded} 本")}}}
        catch(e:Exception){if(e !is CancellationException)mutable.update {it.copy(error=e.message)}}
    }}
    fun applicationForeground(value:Boolean){
        CloudRuntime.foreground(getApplication(),value)
        DiagnosticLog.record(getApplication(),if(value)"app.foreground" else "app.background")
        if(!value)viewModelScope.launch {operations.withLock {withContext(NonCancellable){reader?.readingActivity(false,false)}};CloudRuntime.exit(getApplication())}
    }
    fun loadStatisticsPeriod(kind:String,offset:Int){
        val token=++statsGeneration;mutable.update {it.copy(statsKind=kind,statsOffset=offset,statsLoading=true)}
        viewModelScope.launch {try{val value=core.statisticsPeriod(kind,offset);if(token==statsGeneration)mutable.update {it.copy(statsPeriod=value)}}
            catch(e:Exception){if(token==statsGeneration)mutable.update {it.copy(error=e.message)}}
            finally{if(token==statsGeneration)mutable.update {it.copy(statsLoading=false)}}}
    }
    fun selectStatisticsBook(id:String){mutable.update {it.copy(statsBookId=id,bookStatistics=null)}}
    fun loadBookStatistics(id:String){
        val token=++statsGeneration;mutable.update {it.copy(bookStatistics=null,statsLoading=true)}
        viewModelScope.launch {try{val detail=core.bookStatistics(id);if(token==statsGeneration)mutable.update {it.copy(bookStatistics=detail)}}
            catch(e:Exception){if(token==statsGeneration)mutable.update {it.copy(error=e.message)}}
            finally{if(token==statsGeneration)mutable.update {it.copy(statsLoading=false)}}}
    }
    fun updateSource(source:String){updateRequest?.cancel();updateGeneration++;mutable.update {it.copy(updateSource=source,appUpdate=null,updateChecked=false,downloadedUpdate=null,updateError=null)};viewModelScope.launch(Dispatchers.IO){getApplication<Application>().getSharedPreferences("interface",0).edit().putString("updateSource",source).apply()}}
    fun checkUpdates(){
        val token=++updateGeneration;val snapshot=state.value;mutable.update {it.copy(checkingUpdate=true,updateChecked=false,updateError=null,appUpdate=null,downloadedUpdate=null)}
        viewModelScope.launch {try{val update=core.checkUpdate(snapshot.appVersion,android.os.Build.SUPPORTED_ABIS.first(),snapshot.updateSource);if(token==updateGeneration)mutable.update {it.copy(appUpdate=update,updateChecked=true)}}
            catch(e:Exception){if(token==updateGeneration)mutable.update {it.copy(updateError=e.message)}}
            finally{if(token==updateGeneration)mutable.update {it.copy(checkingUpdate=false)}}}
    }
    fun downloadUpdate(){
        val update=state.value.appUpdate ?: return;val token=updateGeneration;val request=ChatRequest();updateRequest=request
        request.observeAi(object:AiProgressListener {override fun update(stage:String,completed:UInt,total:UInt){if(token==updateGeneration)mutable.update {it.copy(updateProgress=completed)}}})
        mutable.update {it.copy(downloadingUpdate=true,updateError=null,updateProgress=0u)}
        viewModelScope.launch {try{val path=withContext(NonCancellable){core.downloadUpdate(update.token,android.os.Build.SUPPORTED_ABIS.first(),request)};if(token==updateGeneration)mutable.update {it.copy(downloadedUpdate=path)}}
            catch(e:Exception){if(token==updateGeneration)mutable.update {it.copy(updateError=e.message)}}
            finally{request.close();if(updateRequest===request)updateRequest=null;mutable.update {it.copy(downloadingUpdate=false)}}}
    }
    fun cancelUpdate(){updateRequest?.cancel()}
    fun prepareInstall(context:android.content.Context){val path=state.value.downloadedUpdate ?: return
        viewModelScope.launch {try{withContext(Dispatchers.IO){verifyDownloadedApk(context,path)};installDownloadedApk(context,path)}catch(e:Exception){mutable.update {it.copy(updateError=e.message)}}}
    }
    fun exportDiagnostics(context:android.content.Context){viewModelScope.launch {
        try{val info=withContext(Dispatchers.IO){deviceDiagnostics(context)};val path=core.exportDiagnostics(info);mutable.update {it.copy(diagnosticFile=path)};shareDiagnostics(context,path)}catch(e:Exception){mutable.update {it.copy(error=e.message)}}
    }}
    fun downloadFonts() = runOperation {
        core.downloadReadingFonts();val fonts=core.fontFamilies();mutable.update { it.copy(notice="新字体已下载完成",fontFamilies=fonts) }
        reader?.let {session->state.value.reading?.let {publish(session.applyReadingOptions(it))}}
    }
    fun translate(enabled: Boolean) {
        val token=generation
        aiOperation { session,request ->
            val options=core.readingOptions().copy(translated=enabled);core.saveReadingOptions(options)
            if(token==generation)mutable.update {it.copy(reading=options,translated=enabled,aiPaused=false)}
            session.selectionMode(if(enabled) "paragraph" else state.value.selectionMode)
            session.translatePageCancellable(enabled,request).also { if(token==generation&&aiRequest===request)mutable.update { it.copy(translated=enabled) } }
        }
    }
    fun recognize(){val token=generation;aiOperation {session,request->session.recognizePageCancellable(request).also {markUnified(token,request)}}}
    private suspend fun markUnified(token:Long,request:ChatRequest){
        if(token!=generation||aiRequest!==request)return
        withContext(Dispatchers.IO){check(getApplication<Application>().getSharedPreferences("reading",0).edit().putBoolean("unified",true).commit())}
        if(token==generation&&aiRequest===request)mutable.update {it.copy(unified=true)}
    }
    fun cancelAi(userInitiated:Boolean=false){val request=aiRequest;aiRequest=null;request?.cancel();if(userInitiated)mutable.update {it.copy(aiPaused=true)}}
    fun retryAi(){processedPages.clear();prefetchKey=null;mutable.update {it.copy(aiError=null,aiPaused=false)};processVisible()}
    private fun aiOperation(block:suspend(MobileReader,ChatRequest)->MobilePage){
        cancelAi()
        val token=generation;val request=ChatRequest();aiRequest=request
        request.observeAi(object:AiProgressListener {override fun update(stage:String,completed:UInt,total:UInt){if(token==generation&&aiRequest===request)mutable.update {it.copy(aiStage=stage,aiCompleted=completed,aiTotal=total)}}})
        runOperation {
            try{
                if(token!=generation)return@runOperation
                mutable.update {it.copy(processing=true,aiError=null,aiStage="preparing",aiCompleted=0u,aiTotal=0u)}
                reader?.let {
                    val page=block(it,request)
                    if(aiRequest===request&&token==generation){
                        publish(page,token)
                        val toc=it.translateTocCancellable(request)
                        if(aiRequest===request&&token==generation)mutable.update {state->state.copy(toc=toc)}
                    } else page.close()
                }
            }catch(e:Exception){if(token==generation&&!e.message.orEmpty().contains("已停止"))mutable.update {it.copy(aiError=e.message)}}
            finally{request.close();if(aiRequest===request)aiRequest=null;if(token==generation)mutable.update {it.copy(processing=false)}}
        }
    }
    fun automaticLayout(enabled:Boolean){
        val options=state.value.aiOptions ?: return
        if(enabled&&(options.layoutModel.isBlank()||options.layoutProvider.isBlank())){mutable.update {it.copy(error="请先设置排版模型")};return}
        saveAiOptions(options.copy(layoutEnabled=enabled))
    }
    fun processVisible(){
        val snapshot=state.value;val page=snapshot.frame?.info ?: return
        if(snapshot.active==null||(!snapshot.autoLayout&&!snapshot.translated)||snapshot.chatWorking||snapshot.aiPaused||snapshot.selected!=null||snapshot.aiError!=null)return
        val key="${snapshot.active.id}:${page.section}:${page.page}:${snapshot.fontSize}:${snapshot.unified}:${snapshot.autoLayout}:${snapshot.translated}:${page.regions.map {it.source}.hashCode()}"
        if(!processedPages.add(key))return
        while(processedPages.size>64)processedPages.remove(processedPages.first())
        val token=generation
        aiOperation {session,request->
            var page:MobilePage?=null
            try{
                if(snapshot.translated)page=session.translatePageCancellable(true,request)
                if(snapshot.autoLayout){page?.close();page=null;page=session.recognizePageCancellable(request);markUnified(token,request)}
                if(snapshot.reading?.aiPrefetch==true&&prefetchKey!=key){
                    val result=page;page=null
                    if(result!=null&&aiRequest===request&&token==generation)publish(result,token) else result?.close()
                    session.prefetchAi(snapshot.translated,snapshot.autoLayout,request);prefetchKey=key
                    page=session.page()
                }
                page ?: session.page()
            }catch(e:Exception){page?.close();if(e.message.orEmpty().contains("已停止"))processedPages.remove(key);throw e}
        }
    }
    fun trimMemory() = runOperation { reader?.trimMemory() }
    // Caller runs on the I/O dispatcher; provider metadata is never queried by painting.
    private fun documentName(uri:Uri,fallback:String):String = getApplication<Application>().contentResolver.query(uri,arrayOf(OpenableColumns.DISPLAY_NAME),null,null,null)?.use {cursor->if(cursor.moveToFirst())cursor.getString(0) else null}?.takeIf {it.isNotBlank()} ?: fallback
    fun importBook(uri: Uri) = runOperation {
        val app = getApplication<Application>()
        val imported = withContext(Dispatchers.IO) {
            val name = documentName(uri,"book.epub")
            val dir = File(app.cacheDir, "imports").apply { mkdirs() }
            val file = File(dir, name.substringAfterLast('/').substringAfterLast('\\').ifBlank { "book.epub" })
            try {
                app.contentResolver.openInputStream(uri).use { input ->
                    requireNotNull(input) { "无法读取文件" }
                    file.outputStream().use { out ->
                        val buffer = ByteArray(64 * 1024); var total = 0L
                        while (true) {
                            val n = input.read(buffer); if (n < 0) break
                            total += n; require(total <= 256L * 1024 * 1024) { "书籍超过 256 MiB" }
                            out.write(buffer, 0, n)
                        }
                    }
                }; file
            } catch (e: Exception) { file.delete(); throw e }
        }
        try { val books = core.importBook(imported.absolutePath); mutable.update { it.copy(books = books) };CloudRuntime.libraryChanged(getApplication()) }
        finally { withContext(Dispatchers.IO) { imported.delete() } }
    }
    fun open(book: BookSummary) {
        cancelChat();cancelAi();processedPages.clear();prefetchKey=null;scrollPending=0f;selectionPending=null;selectionHandle=false
        generation++
        mutable.update { it.copy(active = book, frame = null, interaction=null,chromeVisible=true,processing=false, hits = emptyList(), selected = null, messages=emptyList(),changes=emptyList(),annotations=emptyList(),translated=state.value.reading?.translated?:false,aiPaused=false,aiError=null,chatWorking=false,chatQuestion="",chatContent="",chatDraft="",draftImages=emptyList(),sendingImages=emptyList()) }
        viewport = null
    }
    fun measure(width: Int, height: Int, density: Float) {
        if (width <= 0 || height <= 0) return
        val next = Viewport((width / density).toUInt(), (height / density).toUInt(), density.coerceIn(1f, 4f))
        if (next == viewport) return
        viewport = next
        resizeJob?.cancel()
        val token = generation
        resizeJob = viewModelScope.launch {
            delay(100)
            operations.withLock {
                if (token != generation) return@withLock
                mutable.update { it.copy(busy = true) }
                try {
                    // Keep native operations serialized even when resizing is
                    // superseded: cancelling a coroutine cannot stop blocking Rust.
                    withContext(NonCancellable) {
                        val book = state.value.active ?: return@withContext
                        val current = reader
                        if (current == null || readerBook != book.id) {
                            current?.shutdown(); current?.close(); reader = null; readerBook = null
                            val opened = core.openBook(book.id, next)
                            if (token != generation) { opened.shutdown(); opened.close(); return@withContext }
                            reader = opened; readerBook = book.id
                            opened.focusMode(state.value.focus).close()
                            opened.selectionMode(state.value.selectionMode)
                            val toc = opened.toc()
                            mutable.update { it.copy(toc = toc) }
                            publish(opened.configure(state.value.fontSize,state.value.unified,state.value.dark), token)
                        } else publish(current.resize(next), token)
                    }
                } catch (e: Exception) { mutable.update { it.copy(error = e.message) } }
                finally { mutable.update { it.copy(busy = false) } }
            }
        }
    }
    private suspend fun nativeFrame(page:MobilePage):ReaderFrame = withContext(Dispatchers.Default) {
        page.use {
            val bitmap=Bitmap.createBitmap(page.width().toInt(),page.height().toInt(),Bitmap.Config.ARGB_8888)
            check(NativePixels.copyPage(page.id().toLong(),bitmap)){"页面绘制失败"}
            ReaderFrame(bitmap,page.info())
        }
    }
    private suspend fun publish(page: MobilePage, token: Long = generation) {
        val frame=nativeFrame(page)
        if(token==generation)mutable.update {it.copy(frame=frame,selected=null)}
    }
    fun dismissInteraction(){mutable.update {it.copy(interaction=null)}}
    fun focus(enabled:Boolean){
        val token=generation
        readerOperation { session->session.focusMode(enabled).also {
            withContext(Dispatchers.IO){check(getApplication<Application>().getSharedPreferences("reading",0).edit().putBoolean("focus",enabled).commit())}
            if(token==generation)mutable.update {it.copy(focus=enabled)}
        } }
    }
    fun followLink(target:String)=readerOperation {it.followLink(target)}
    fun returnLocation()=readerOperation {it.goBack()}
    fun tap(x:Float,y:Float){
        if(state.value.chatWorking)return
        if(state.value.processing){cancelAi();return}
        if(state.value.busy)return
        if(state.value.selected!=null){clearSelection();return}
        val token=generation
        runOperation { if(token==generation)reader?.let {session->
            val result=session.activate(x,y)
            val frame=result.media?.let {nativeFrame(it)}
            if(token!=generation)return@let
            when(result.kind){
                "page"->if(frame!=null)mutable.update {it.copy(frame=frame,selected=null)}
                "none"->{val width=viewport?.width?.toFloat() ?: 360f
                    if(x<width*.3f||x>width*.7f)publish(session.turn(x>width*.7f),token)
                    else mutable.update {it.copy(chromeVisible=!it.chromeVisible)}
                }
                else->{
                    val notes=if(result.kind=="annotation")session.annotations() else state.value.annotations
                    if(token==generation)mutable.update {it.copy(annotations=notes,interaction=ReaderInteraction(result.kind,result.text,result.target,frame,result.markup))}
                }
            }
        } }
    }
    private fun readerOperation(block: suspend (MobileReader) -> MobilePage) {
        cancelAi()
        val token=generation
        runOperation { if(token==generation) reader?.let { it.readingActivity(true,true); publish(block(it),token) } }
    }
    fun turn(forward: Boolean) = readerOperation { it.turn(forward) }
    fun scroll(delta:Float){
        if(state.value.chatWorking)return
        cancelAi();scrollPending+=delta
        if(scrollJob?.isActive==true)return
        val token=generation
        scrollJob=viewModelScope.launch {
            while(scrollPending!=0f&&token==generation){
                val amount=scrollPending;scrollPending=0f
                operations.withLock {withContext(NonCancellable){reader?.let {session->
                    try {val frame=nativeFrame(session.scrollBy(amount));val selected=session.selection();if(token==generation)mutable.update {it.copy(frame=frame,selected=selected)}}
                    catch(e:Exception){if(token==generation)mutable.update {it.copy(error=e.message)}}
                }}}
            }
        }
    }
    fun navigate(href: String) = readerOperation { it.navigate(href) }
    fun configure(size: Float, unified: Boolean, dark: Boolean) {
        val token=generation
        runOperation {
            if(token!=generation) return@runOperation
            reader?.let { publish(it.configure(size,unified,dark),token) }
            withContext(Dispatchers.IO) { check(getApplication<Application>().getSharedPreferences("reading",0).edit().putFloat("size",size).putBoolean("unified",unified).putBoolean("dark",dark).commit()) }
            mutable.update { it.copy(fontSize=size,unified=unified,dark=dark) }
        }
    }
    fun search(query: String) {
        val token=generation
        runOperation { if(token==generation) { val hits=reader?.search(query).orEmpty(); if(token==generation) mutable.update { it.copy(hits=hits) } } }
    }
    fun jump(hit: SearchHit) = readerOperation { it.goToSource(hit.source) }
    fun beginHandle(start:Boolean){selectionHandle=true;val token=generation;runOperation {if(token==generation)reader?.beginSelectionHandle(start)}}
    fun dragHandle(x:Float,y:Float){selectionHandle=true;select(x,y,x,y)}
    fun select(x: Float, y: Float, endX: Float, endY: Float) {
        cancelAi();selectionPending=floatArrayOf(x,y,endX,endY,if(selectionHandle)1f else 0f)
        if(selectionJob?.isActive==true)return
        val token=generation
        selectionJob=viewModelScope.launch {
            while(token==generation){val points=selectionPending ?: break;selectionPending=null
                operations.withLock {withContext(NonCancellable){reader?.let {session->
                    val selected=if(points[4]==1f)session.extendSelection(points[2],points[3]) else session.select(points[0],points[1],points[2],points[3])
                    if(token==generation)mutable.update {it.copy(selected=selected)}
                }}}
            }
        }
    }
    fun endHandle(){selectionHandle=false}
    fun selectionMode(mode: String) {
        val token=generation
        runOperation { if(token==generation) { reader?.selectionMode(mode);if(token==generation)mutable.update { it.copy(selectionMode=mode) } } }
    }
    fun clearSelection() {
        selectionPending=null;selectionHandle=false
        val token=generation
        mutable.update { it.copy(selected = null) }
        runOperation { if(token==generation)reader?.clearSelection() }
    }
    fun back() {
        cancelChat();cancelAi();processedPages.clear();prefetchKey=null;scrollPending=0f;selectionPending=null;selectionHandle=false
        generation++
        resizeJob?.cancel(); viewport = null
        mutable.update { it.copy(active = null, frame = null, interaction=null,chromeVisible=true,processing=false, selected = null, toc = emptyList(),chatWorking=false,chatQuestion="",chatContent="",chatDraft="",draftImages=emptyList(),sendingImages=emptyList()) }
        runOperation { reader?.shutdown(); reader?.close(); reader = null; readerBook = null; val books=core.books(); mutable.update { it.copy(books=books) } }
    }
    fun interfaceSettings(theme: String, language: String) = runOperation {
        withContext(Dispatchers.IO) { check(getApplication<Application>().getSharedPreferences("interface",0).edit().putString("theme",theme).putString("language",language).commit()) }
        mutable.update { it.copy(appTheme=theme,language=language) }
    }
    fun removeBook(book: BookSummary) = runOperation { core.removeBook(book.id);CloudRuntime.libraryChanged(getApplication()); covers.remove(book.id); missingCovers.remove(book.id); val books=core.books(); mutable.update { it.copy(books=books) } }
    fun loadStatistics() = runOperation { val statistics=core.statistics(); mutable.update { it.copy(statistics=statistics) } }
    fun loadAnnotations() {
        val token=generation
        runOperation { if(token==generation) { val notes=reader?.annotations().orEmpty(); if(token==generation) mutable.update { it.copy(annotations=notes) } } }
    }
    private fun annotationOperation(action: suspend (MobileReader) -> List<Annotation>) {
        val token=generation
        runOperation { if(token==generation) reader?.let { r -> val notes=action(r);publish(r.page(),token);if(token==generation)mutable.update { it.copy(annotations=notes) } } }
    }
    fun saveAnnotation(note: String?) = annotationOperation { it.saveAnnotation(note?.trim()?.takeIf(String::isNotEmpty)) }
    fun editAnnotation(id: String,note: String?) = annotationOperation { it.editAnnotation(id,note?.trim()?.takeIf(String::isNotEmpty)) }
    fun deleteAnnotation(id: String) = annotationOperation { it.deleteAnnotation(id) }
    fun jumpAnnotation(note: Annotation) = readerOperation { it.goToSource(note.source) }
    fun markFinished() = runOperation { reader?.markFinished(); mutable.update { it.copy(notice="已标记为读完") } }
    suspend fun readingActivity(eligible: Boolean, activity: Boolean = false) {
        val token=generation
        operations.withLock { if(token==generation) withContext(NonCancellable) { reader?.readingActivity(eligible,activity) } }
    }
    fun loadChat() {
        val token=generation
        runOperation { if(token==generation) { val messages=reader?.chatHistory().orEmpty(); if(token==generation)mutable.update { it.copy(messages=messages) } } }
    }
    fun editChatDraft(text:String){mutable.update {it.copy(chatDraft=text)}}
    fun attachImages(uris:List<Uri>){
        val token=generation
        runOperation {
            check(state.value.draftImages.size+uris.size<=8){"一条消息最多附带 8 张图片"}
            val session=reader ?: return@runOperation
            for(uri in uris){
                if(token!=generation)return@runOperation
                val (temp,label)=withContext(Dispatchers.IO){
                    val resolver=getApplication<Application>().contentResolver
                    val file=File.createTempFile("chat-image-",".bin",getApplication<Application>().cacheDir)
                    try {
                        resolver.openInputStream(uri).use {input->checkNotNull(input){"无法打开图片"};file.outputStream().use {output->
                            val buffer=ByteArray(65536);var total=0L
                            while(true){val count=input.read(buffer);if(count<0)break;total+=count;check(total<=20*1024*1024){"图片文件超过 20 MB"};output.write(buffer,0,count)}
                        }}
                        file to documentName(uri,"Image")
                    }catch(e:Exception){file.delete();throw e}
                }
                try {
                    val attachment=session.addChatImage(temp.absolutePath,label)
                    if(token==generation)mutable.update {it.copy(draftImages=(it.draftImages+attachment).distinctBy {a->a.id})}
                }finally{withContext(Dispatchers.IO){temp.delete()}}
            }
        }
    }
    fun attachBookImages(source:String){
        val token=generation
        runOperation {
            val session=reader ?: return@runOperation
            val attached=session.addChatBookImages(source)
            check(state.value.draftImages.size+attached.size<=8){"一条消息最多附带 8 张图片"}
            if(token==generation)mutable.update {it.copy(draftImages=(it.draftImages+attached).distinctBy {a->a.id})}
        }
    }
    fun removeChatImage(id:String){val token=generation;runOperation {if(token==generation){reader?.removeChatImage(id);mutable.update {it.copy(draftImages=it.draftImages.filterNot {a->a.id==id})}}}}
    fun previewChatImage(id:String){val token=generation;runOperation {if(token==generation)reader?.let {session->val frame=nativeFrame(session.chatImage(id));if(token==generation)mutable.update {it.copy(interaction=ReaderInteraction("image","","",frame))}}}}
    fun followChatCitation(target:String)=readerOperation {it.followChatCitation(target)}
    fun sendChat(question: String) {
        val images=state.value.draftImages
        if(state.value.chatWorking || (question.isBlank()&&images.isEmpty())) return
        cancelAi()
        val token=generation
        val request=ChatRequest();chatRequest=request
        mutable.update { it.copy(chatWorking=true,chatQuestion=question,chatContent="",chatReasoning="",chatActivity="",chatDraft="",draftImages=emptyList(),sendingImages=images) }
        chatJob=viewModelScope.launch {
            operations.withLock {
                try { withContext(NonCancellable) {
                    if(token!=generation) return@withContext
                    val reply=reader?.chatWithImages(question,images,request,object: ChatListener {
                        override fun update(content: String,reasoning: String,activity: String) {
                            if(token==generation) mutable.update { it.copy(chatContent=content,chatReasoning=reasoning,chatActivity=activity) }
                        }
                    }) ?: return@withContext
                    if(token==generation)mutable.update { it.copy(messages=reply.messages,changes=reply.changes) }
                } } catch(e: Exception) {
                    if(token==generation)mutable.update {it.copy(chatDraft=question,draftImages=images,error=if(e.message.orEmpty().contains("已停止"))it.error else e.message)}
                } finally { request.close();if(chatRequest===request)chatRequest=null;if(token==generation)mutable.update { it.copy(chatWorking=false,chatQuestion="",chatContent="",chatActivity="",chatReasoning="",sendingImages=emptyList()) } }
            }
        }
    }
    fun cancelChat() { chatRequest?.cancel() }
    fun resolveChanges(accept: Boolean) {
        val token=generation
        readerOperation { session -> session.resolveChatChanges(accept).also { if(token==generation)mutable.update { it.copy(changes=emptyList()) } } }
    }
    fun undoRewrites() = readerOperation { it.undoRewrites() }
    fun clearChat() {
        val token=generation
        runOperation { if(token==generation) { reader?.clearChat();if(token==generation)mutable.update { it.copy(messages=emptyList(),changes=emptyList(),draftImages=emptyList(),sendingImages=emptyList()) } } }
    }
    override fun onCleared() {
        // ViewModel survives rotation; closing the owning activity releases the
        // native session on a worker, not in the UI lifecycle callback.
        cancelChat();cancelAi();cancelUpdate();processedPages.clear();prefetchKey=null;scrollPending=0f;selectionPending=null;selectionHandle=false
        generation++
        CoroutineScope(Dispatchers.IO).launch {
            operations.withLock { reader?.shutdown(); reader?.close(); reader = null }
        }
    }
}

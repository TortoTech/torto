package org.tortotech.torto

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.viewModels
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.*
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalView
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.repeatOnLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import kotlinx.coroutines.*
import org.tortotech.torto.core.Annotation
import org.tortotech.torto.core.BookSummary
import org.tortotech.torto.core.Provider

class MainActivity : ComponentActivity() {
    private val readerModel: ReaderModel by viewModels()
    override fun onTrimMemory(level: Int) {
        super.onTrimMemory(level)
        if (level >= android.content.ComponentCallbacks2.TRIM_MEMORY_UI_HIDDEN) readerModel.trimMemory()
    }
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState); enableEdgeToEdge()
        setContent { TortoApp(readerModel) }
    }
}
/** Fast cached reader operations need no transient loading UI. Finishing hides it immediately. */
@Composable internal fun loadingVisible(active:Boolean,quietUpdate:Boolean=false,aiProcessing:Boolean=false):Boolean {
    val requested=active&&!quietUpdate&&!aiProcessing
    var visible by remember(requested){mutableStateOf(false)}
    LaunchedEffect(requested){if(requested){delay(200);visible=true}}
    return requested&&visible
}
@OptIn(ExperimentalMaterial3Api::class)
@Composable fun TortoApp(model: ReaderModel = viewModel()) {
    val state by model.state.collectAsStateWithLifecycle()
    val feedback:FeedbackModel=viewModel()
    val feedbackState by feedback.state.collectAsStateWithLifecycle()
    val showBusy=loadingVisible(state.busy,state.quietUpdate)
    val choicePages=remember{ChoicePages()}
    val assistantUi=rememberAssistantUiState(state.active?.id)
    LaunchedEffect(state.active?.id){choicePages.close()}
    var tab by rememberSaveable { mutableIntStateOf(0) }
    val routes=remember { mutableStateListOf<String>() }
    var panel by remember { mutableStateOf<String?>(null) }
    var editing by remember { mutableStateOf<Annotation?>(null) }
    var editingProvider by remember { mutableStateOf<Provider?>(null) }
    var bookActions by remember { mutableStateOf<BookSummary?>(null) }
    var deleteBook by remember { mutableStateOf<BookSummary?>(null) }
    val snackbar=remember { SnackbarHostState() }
    val context=LocalContext.current
    val view=LocalView.current
    LaunchedEffect(state.loaded) {
        if(state.loaded) {
            // Let a shelf frame finish before starting discovery or networking.
            withFrameNanos {};withFrameNanos {}
            ReadingFonts.start(context)
            model.checkUpdatesIfNeeded()
        }
    }
    val lifecycle=LocalLifecycleOwner.current.lifecycle
    val lifecycleState by lifecycle.currentStateFlow.collectAsState()
    val window=(context as? android.app.Activity)?.window
    ReadingWindowBrightness(window,state.active!=null,state.readingBrightness)
    val systemBars=remember(window,view){window?.let {WindowCompat.getInsetsController(it,view)}}
    val darkBars=if(state.active!=null)state.dark else when(state.appTheme){"dark"->true;"light"->false;else->isSystemInDarkTheme()}
    DisposableEffect(systemBars){
        val previousBehavior=systemBars?.systemBarsBehavior
        val lightStatus=systemBars?.isAppearanceLightStatusBars
        val lightNavigation=systemBars?.isAppearanceLightNavigationBars
        systemBars?.systemBarsBehavior=WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        onDispose {
            systemBars?.show(WindowInsetsCompat.Type.systemBars())
            previousBehavior?.let {systemBars?.systemBarsBehavior=it}
            lightStatus?.let {systemBars?.isAppearanceLightStatusBars=it}
            lightNavigation?.let {systemBars?.isAppearanceLightNavigationBars=it}
        }
    }
    LaunchedEffect(systemBars,state.active?.id,state.chromeVisible,state.annotationPreviewId,state.completionPage,panel,lifecycleState,darkBars,choicePages.isOpen){
        systemBars?.isAppearanceLightStatusBars=!darkBars
        systemBars?.isAppearanceLightNavigationBars=!darkBars
        if(!choicePages.isOpen&&state.active!=null&&!state.completionPage&&(state.annotationPreviewId!=null||!state.chromeVisible||readingBottomPanel(panel)))systemBars?.hide(WindowInsetsCompat.Type.systemBars())
        else systemBars?.show(WindowInsetsCompat.Type.systemBars())
    }
    val picker=rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()){it?.let(model::importBook)}
    fun open(route:String){ if(route=="ai")editingProvider=null;routes.add(route);if(route in listOf("ai","translation","assistant","sync","layout"))model.loadServices() }
    fun editProvider(provider:Provider){editingProvider=provider;open("provider")}
    fun closeProvider(){if(routes.lastOrNull()=="provider"){routes.removeAt(routes.lastIndex);editingProvider=null}}
    fun reportIssue(){
        val metrics=context.resources.displayMetrics
        feedback.open(model,org.tortotech.torto.core.FeedbackEnvironment("Android ${android.os.Build.VERSION.RELEASE} (API ${android.os.Build.VERSION.SDK_INT})","${android.os.Build.MANUFACTURER} ${android.os.Build.MODEL}",android.os.Build.SUPPORTED_ABIS.firstOrNull() ?: "unknown",metrics.widthPixels.toUInt(),metrics.heightPixels.toUInt(),metrics.density))
    }
    fun closePanel(){if(panel=="chat")model.cancelChat();panel=null;editing=null}
    fun readerPanel(name:String){
        if(readingPanelOwner(panel)==name){closePanel();return}
        when(name){"chat"->{model.hideChrome();model.loadChat()};"marks"->model.loadAnnotations()}
        panel=name
    }
    fun exitReading(){closePanel();routes.clear();tab=0;model.back()}
    fun back(){when{choicePages.isOpen->choicePages.close();state.completionPage->exitReading();readingTypographyParent(panel)!=null->panel=readingTypographyParent(panel);panel=="marks-search"->panel="marks";panel=="note"&&editing!=null->{editing=null;panel="marks"};panel!=null->closePanel();state.annotationPreviewId!=null->{model.loadAnnotations();panel="marks"};state.active!=null->exitReading();routes.lastOrNull()=="provider"->{if(!state.busy)closeProvider()};routes.isNotEmpty()->routes.removeAt(routes.lastIndex);tab!=0->tab=0}}
    BackHandler(panel!=null||state.active!=null||routes.isNotEmpty()||tab!=0){back()}
    LaunchedEffect(state.interaction){
        state.interaction?.let {interaction->when(interaction.kind){
            "external"->{
                val uri=Uri.parse(interaction.target)
                if(uri.scheme in listOf("https","http"))try{context.startActivity(Intent(Intent.ACTION_VIEW,uri))}catch(_:android.content.ActivityNotFoundException){}
                model.dismissInteraction()
            }
            "annotation"->{editing=state.annotations.firstOrNull {it.id==interaction.target};if(editing!=null)panel="note";model.dismissInteraction()}
        }}
    }
    LaunchedEffect(state.active?.id,state.frame?.info?.section,state.frame?.info?.page,state.frame?.info?.regions?.map {it.source},state.autoLayout,state.translated,state.fontSize,state.unified,state.aiOptions,state.reading,state.settingsRevision,state.selected,state.annotationPreviewId,state.completionPage,panel,choicePages.isOpen,feedbackState.visible,state.interaction?.kind,state.chatWorking,state.readerMoving,state.busy,lifecycleState){
        model.processVisible(blocked=lifecycleState!=Lifecycle.State.RESUMED||feedbackState.visible||panel!=null||choicePages.isOpen||state.annotationPreviewId!=null||state.completionPage)
    }
    DisposableEffect(lifecycle){
        model.applicationForeground(lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED))
        val observer=androidx.lifecycle.LifecycleEventObserver {_,event->when(event){Lifecycle.Event.ON_START->model.applicationForeground(true);Lifecycle.Event.ON_STOP->{model.cancelAi();model.cancelChat();model.applicationForeground(false)};else->{}}}
        lifecycle.addObserver(observer);onDispose{lifecycle.removeObserver(observer);model.applicationForeground(false)}
    }
    LaunchedEffect(state.active?.id,panel,choicePages.isOpen,state.busy,state.chatWorking,state.interaction,state.completionPage) {
        if(state.active==null)return@LaunchedEffect
        lifecycle.repeatOnLifecycle(Lifecycle.State.RESUMED) {
            try {
                if(panel==null&&!choicePages.isOpen&&!state.completionPage&&state.interaction==null&&!state.busy&&!state.chatWorking) {
                    model.readingActivity(true,true)
                    while(isActive){delay(1000);model.readingActivity(true)}
                } else awaitCancellation()
            } finally {withContext(NonCancellable){model.readingActivity(false)}}
        }
    }
    TortoTheme(state) {
        ChoicePageHost(choicePages){
        val resumeNotice=state.notice=="发现其他设备进度"&&state.cloudResume!=null
        val fontNotice=state.notice=="新字体已下载完成"
        val notice=state.notice?.let {if(fontNotice)tr(it,"New fonts downloaded")else if(resumeNotice)tr(it,"Reading progress found on another device")else it}
        val applyFonts=fontNotice&&state.active!=null&&state.fontApplyAvailable
        val applyLabel=tr("应用","Apply")
        val resumeLabel=tr("跳转","Go to")
        LaunchedEffect(notice,if(fontNotice)state.fontNoticeUntil else 0,if(fontNotice)state.active?.id else null,applyFonts) {
            notice?.let {
                val result=if(fontNotice)withTimeoutOrNull((state.fontNoticeUntil-android.os.SystemClock.uptimeMillis()).coerceAtLeast(0)) {
                    snackbar.showSnackbar(it,actionLabel=if(applyFonts)applyLabel else null,duration=SnackbarDuration.Indefinite)
                }else snackbar.showSnackbar(it,actionLabel=if(resumeNotice)resumeLabel else null)
                if(result==SnackbarResult.ActionPerformed&&resumeNotice)model.resumeCloudProgress()
                if(result==SnackbarResult.ActionPerformed&&fontNotice)model.applyDownloadedFonts()
                model.clearNotice(state.notice,if(fontNotice)state.fontNoticeUntil else null)
            }
        }
        val safe=readerSafePadding()
        val route=routes.lastOrNull()
        val title=when(route){
            "settings"->tr("设置","Settings");"typography"->tr("排版","Typesetting")
            "dark-mode"->tr("深色模式","Dark mode");"language"->tr("界面语言","Interface language")
            "layout"->tr("AI 排版","AI layout");"ai"->tr("AI 提供商","AI providers");"translation"->tr("翻译","Translation");"assistant"->tr("阅读助手","Reading assistant")
            "provider"->if(editingProvider?.id.isNullOrEmpty())tr("添加 AI 提供商","Add AI provider")else tr("编辑 AI 提供商","Edit AI provider")
            "sync"->tr("云同步","Cloud sync");"about"->tr("关于","About");"statistics"->tr("阅读统计","Reading statistics");"book-statistics"->tr("阅读详情","Reading details")
            else->if(tab==0)tr("书架","Library") else tr("我","Me")
        }
        ReaderEntryHost(state){state->
        Scaffold(
            containerColor=if(state.active!=null)readerPaperColor(state)else if(route=="about")MaterialTheme.colorScheme.surfaceContainerLowest else MaterialTheme.colorScheme.background,
            contentWindowInsets=if(state.active!=null)WindowInsets(0,0,0,0)else ScaffoldDefaults.contentWindowInsets,
            snackbarHost={SnackbarHost(snackbar)},
            topBar={if(state.active==null&&route in listOf("settings","dark-mode","language","typography","ai","provider","layout","translation","assistant","sync","about","statistics","book-statistics"))CenterAlignedTopAppBar(
                title={Text(title,fontSize=16.sp)},
                navigationIcon={ActionIcon("back",tr("返回","Back"),route!="provider"||!state.busy){back()}},
                actions={if(route=="ai")ActionIcon("add",tr("添加提供商","Add provider"),!state.busy){editProvider(Provider("","custom","","",emptyList(),false))}},
                colors=TopAppBarDefaults.topAppBarColors(containerColor=if(route=="about")MaterialTheme.colorScheme.surfaceContainerLowest else MaterialTheme.colorScheme.surfaceContainerLow)
            )else if(state.active==null)TopAppBar(
                title={if(!(route==null&&tab==1))Text(title,fontSize=if(route==null&&tab==0)20.sp else androidx.compose.ui.unit.TextUnit.Unspecified)},
                navigationIcon={
                    if(route==null&&tab==1)ActionIcon("feedback",tr("反馈问题","Report an issue"),onClick=::reportIssue)
                    else if(route!=null)ActionIcon("back",tr("返回","Back")){back()}
                },
                actions={
                    if(route==null&&tab==0){
                        ActionIcon("add",tr("导入书籍","Import book"),!state.busy){picker.launch(arrayOf("*/*"))}
                    }else if(route==null&&tab==1){
                        ActionIcon("settings",tr("设置","Settings")){open("settings")}
                    }
                },colors=TopAppBarDefaults.topAppBarColors(containerColor=MaterialTheme.colorScheme.background)
            )},
            bottomBar={if(state.active==null&&route==null)CompactHomeBar(tab){tab=it;if(it==1)model.loadStatistics()}}
        ){padding->
            Box(Modifier.fillMaxSize().padding(if(state.active==null)padding else PaddingValues(0.dp))) {
                if(state.active!=null) Box(Modifier.fillMaxSize().padding(readerContentPadding(state.focus))) {
                    Box(Modifier.fillMaxSize()){
                    AndroidView(factory={ReaderView(it).apply {animateOpening=true;onSize=model::measure;onMotion=model::readerMotion;onContentLoading=model::readerContentLoading;onReadingGesture=model::hideChrome;onTap=model::tap;onTurn=model::turn;onUnitTurn=model::turnUnit;onScroll=model::scroll;onSentenceGesture=model::sentenceGesture;onHandleStart=model::beginHandle;onHandleMove=model::dragHandle;onHandleEnd=model::endHandle;onSelect={x,y,ex,ey->if(!model.state.value.chatWorking&&model.state.value.annotationPreviewId==null)model.select(x,y,ex,ey)}}},update={it.isEnabled=!state.chatWorking&&!feedbackState.visible&&panel==null&&!choicePages.isOpen&&!state.completionPage;it.importantForAccessibility=if(panel!=null||choicePages.isOpen||feedbackState.visible||state.completionPage)android.view.View.IMPORTANT_FOR_ACCESSIBILITY_NO_HIDE_DESCENDANTS else android.view.View.IMPORTANT_FOR_ACCESSIBILITY_AUTO;if(state.error!=null)it.cancelPendingNavigation();it.present(state.frame,state.selected,state.previousPage,state.nextPage,state.turnCompletion);it.presentReference(state.interaction?.indicator,org.tortotech.torto.core.referenceColor(state.dark))},modifier=Modifier.fillMaxSize())
                    ReaderOpeningIndicator(state)
                    }
                    state.frame?.info?.controls?.filter {it.kind!="sentences"&&state.annotationPreviewId==null&&!state.completionPage}?.takeIf {it.isNotEmpty()}?.let {controls->
                        Row(Modifier.align(Alignment.BottomCenter).padding(bottom=if(state.chromeVisible)ReaderBarHeight else 0.dp).fillMaxWidth().horizontalScroll(rememberScrollState()).background(MaterialTheme.colorScheme.surfaceContainerLow),horizontalArrangement=Arrangement.Center){
                            controls.forEach {control->TextButton(onClick={model.toggleReadingControl(control)},enabled=!state.busy&&!state.readerMoving){
                                Text(if(control.kind=="formula")if(control.active)tr("收起公式备注","Hide formula notes") else tr("公式备注","Formula notes")
                                    else if(control.active)tr("显示表格","Show table") else tr("显示图表","Show chart"))
                            }}
                        }
                    }
                    if(!state.completionPage&&panel==null&&state.annotationPreviewId==null&&state.interaction==null&&!state.readerMoving)state.selected?.let {selection->
                        SelectionToolbar(selection,
                            onCopy={context.getSystemService(ClipboardManager::class.java).setPrimaryClip(ClipData.newPlainText("",selection.text));model.clearSelection()},
                            onDismiss=model::clearSelection,
                            onHighlight=model::toggleSelectionHighlight,
                            onNote={editing=null;panel="note"})
                    }
                } else when(route){
                    "settings"->SettingsScreen(state,::open)
                    "dark-mode"->DarkModeScreen(state,model)
                    "language"->InterfaceLanguageScreen(state,model)
                    "typography"->TypographyPanel(state,model,openLayout={open("layout")})
                    "ai"->ProviderManager(state,model,::editProvider)
                    "provider"->editingProvider?.let {ProviderEditor(it,state,model,::closeProvider)}
                    "translation","assistant","sync","layout"->ServiceSettingsPanel(state,model,route)
                    "statistics"->StatisticsScreen(state,model){id->model.selectStatisticsBook(id);open("book-statistics")}
                    "book-statistics"->BookStatisticsScreen(state,model)
                    "about"->AboutScreen(state,model)
                    else->if(tab==0)ShelfScreen(state,model){bookActions=it} else ProfileScreen(state){open("statistics")}
                }
                if(state.active!=null){
                    ReadingPanelHost(state.active?.id,panel,::closePanel){name->
                        when(name){
                            "navigation"->ReadingNavigation(state,panel=="navigation",model,::closePanel)
                            "typography"->ReadingTypographyPanel(state,model){next->panel=next;if(next=="layout-settings")model.loadServices()}
                            "font-style"->ReadingFontStylePanel(state,model,{panel="typography"}){panel="font-choice-${it.name}"}
                            "reading-mode"->ReadingModePanel(state,model){panel="typography"}
                            "selection"->ReadingSelectionPanel(state,model,::closePanel)
                            "theme"->ReadingThemePanel(state,model)
                            "layout-settings"->ReadingPanelPage(tr("AI 排版设置","AI layout settings"),{panel="typography"},fill=false){ServiceSettingsPanel(state,model,"layout")}
                            "translation-settings"->ReadingPanelPage(tr("翻译设置","Translation settings"),{panel="typography"}){ServiceSettingsPanel(state,model,"translation")}
                            "marks"->AnnotationsPanel(state,model,{panel="marks-search"},::closePanel)
                            "marks-search"->NotesSearchPanel(state,model,{panel="marks"},::closePanel)
                            else->if(name.startsWith("font-choice-"))ReadingFontSetting.entries.firstOrNull {it.name==name.removePrefix("font-choice-")}?.let {ReadingFontPicker(it,state,model){panel="font-style"}}
                        }
                    }
                    if(!state.completionPage&&state.annotationPreviewId!=null&&panel==null)NoteReadingChrome(state,
                        onList={model.loadAnnotations();panel="marks"},onClose=model::exitAnnotationPreview,
                        onPrevious={model.stepAnnotation(false)},onNext={model.stepAnnotation(true)})
                    else if(!state.completionPage)ReadingChrome(state,showBusy||state.chatWorking,panel,::back,::readerPanel,
                        onTranslate={model.translate(!state.translated)},onTheme={readerPanel("theme")},
                        onFeedback=::reportIssue)
                }
                if(state.active!=null&&!state.completionPage&&state.interaction?.kind !in listOf("note","reference"))ReaderAiNotice(state,Modifier.align(Alignment.TopCenter).padding(top=safe.calculateTopPadding()+12.dp))
                if(state.active!=null&&state.completionPage)ReadingCompletionPage(state,model::closeCompletionPage,::exitReading,model::loadCompletion,model::markCompletionFinished)
            }
        }
        }
        if(feedbackState.visible)FeedbackSheet(feedbackState,feedback,if(state.active!=null)state.readingBrightness else -1f)
        if(panel=="chat")ReadingAssistantDialog(state,model,::closePanel,assistantUi)
        if(panel=="note")NoteComposerDialog(state,editing?.quote ?: state.selected?.text.orEmpty(),editing?.note,::closePanel){text,done->
            editing?.let {model.editAnnotation(it.id,text,done)} ?: model.saveAnnotation(text,done)
        }
        state.interaction?.let {interaction->
            if(interaction.kind in listOf("image","formula"))MediaPreview(interaction,model::dismissInteraction)
            else if(interaction.kind in listOf("note","reference"))ReadingReferenceSheet(interaction,state.fontSize,state.dark,model,state)
        }
        bookActions?.let {book->ModalBottomSheet(onDismissRequest={bookActions=null}){
            Text(book.title,Modifier.padding(20.dp),style=MaterialTheme.typography.titleLarge)
            ListItem(headlineContent={Text(tr("阅读","Read"))},leadingContent={AppIcon("book")},modifier=Modifier.clickable{model.open(book);bookActions=null})
            ListItem(headlineContent={Text(tr("阅读详情","Reading details"))},leadingContent={AppIcon("stats")},modifier=Modifier.clickable {model.selectStatisticsBook(book.id);bookActions=null;open("book-statistics")})
            ListItem(headlineContent={Text(tr("删除书籍","Delete book"))},leadingContent={AppIcon("delete")},modifier=Modifier.clickable{deleteBook=book;bookActions=null})
            Spacer(Modifier.navigationBarsPadding())
        }}
        deleteBook?.let {book->AlertDialog(onDismissRequest={deleteBook=null},title={Text(tr("删除书籍？","Delete book?"))},text={Text(book.title)},confirmButton={TextButton(onClick={model.removeBook(book);deleteBook=null}){Text(tr("删除","Delete"))}},dismissButton={TextButton(onClick={deleteBook=null}){Text(tr("取消","Cancel"))}})}
        state.error?.let {message->AlertDialog(onDismissRequest=model::clearError,title={Text(tr("暂时无法完成","Unable to complete"))},text={Text(message)},confirmButton={TextButton(onClick=model::clearError){Text(tr("知道了","OK"))}})}
        }
    }
}

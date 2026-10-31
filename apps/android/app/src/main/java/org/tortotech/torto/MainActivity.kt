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
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.repeatOnLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import kotlinx.coroutines.*
import org.tortotech.torto.core.Annotation
import org.tortotech.torto.core.BookSummary

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
@OptIn(ExperimentalMaterial3Api::class)
@Composable fun TortoApp(model: ReaderModel = viewModel()) {
    val state by model.state.collectAsStateWithLifecycle()
    var tab by rememberSaveable { mutableIntStateOf(0) }
    val routes=remember { mutableStateListOf<String>() }
    var panel by remember { mutableStateOf<String?>(null) }
    var query by remember { mutableStateOf("") }
    var editing by remember { mutableStateOf<Annotation?>(null) }
    var bookActions by remember { mutableStateOf<BookSummary?>(null) }
    var deleteBook by remember { mutableStateOf<BookSummary?>(null) }
    val snackbar=remember { SnackbarHostState() }
    val context=LocalContext.current
    val view=LocalView.current
    val lifecycle=LocalLifecycleOwner.current.lifecycle
    val lifecycleState by lifecycle.currentStateFlow.collectAsState()
    DisposableEffect(state.active?.id,state.chromeVisible){
        val window=(context as? android.app.Activity)?.window
        val controller=window?.let {WindowCompat.getInsetsController(it,view)}
        if(state.active!=null&&!state.chromeVisible)controller?.hide(WindowInsetsCompat.Type.systemBars()) else controller?.show(WindowInsetsCompat.Type.systemBars())
        onDispose {controller?.show(WindowInsetsCompat.Type.systemBars())}
    }
    val picker=rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()){it?.let(model::importBook)}
    fun open(route:String){ routes.add(route);if(route in listOf("ai","translation","assistant","sync","layout"))model.loadServices();if(route=="statistics")model.loadStatistics() }
    fun closePanel(){if(panel=="chat")model.cancelChat();panel=null;editing=null}
    fun back(){when{panel!=null->closePanel();state.active!=null->if(state.frame?.info?.canGoBack==true)model.returnLocation() else model.back();routes.isNotEmpty()->routes.removeAt(routes.lastIndex);tab!=0->tab=0}}
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
    LaunchedEffect(state.active?.id,state.frame?.info?.section,state.frame?.info?.page,state.frame?.info?.regions?.map {it.source},state.autoLayout,state.translated,state.fontSize,state.unified,state.aiOptions,state.reading,state.settingsRevision,state.selected,panel,state.interaction?.kind,state.chatWorking,lifecycleState){
        if(lifecycleState==Lifecycle.State.RESUMED&&panel==null&&state.interaction==null&&!state.chatWorking){delay(250);model.processVisible()}
    }
    DisposableEffect(lifecycle){
        model.applicationForeground(lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED))
        val observer=androidx.lifecycle.LifecycleEventObserver {_,event->when(event){Lifecycle.Event.ON_START->model.applicationForeground(true);Lifecycle.Event.ON_STOP->{model.cancelAi();model.cancelChat();model.applicationForeground(false)};else->{}}}
        lifecycle.addObserver(observer);onDispose{lifecycle.removeObserver(observer);model.applicationForeground(false)}
    }
    LaunchedEffect(state.notice){state.notice?.let {snackbar.showSnackbar(it);model.clearNotice()}}
    LaunchedEffect(state.active?.id,panel,state.busy,state.chatWorking,state.interaction) {
        if(state.active==null)return@LaunchedEffect
        lifecycle.repeatOnLifecycle(Lifecycle.State.RESUMED) {
            try {
                if(panel==null&&state.interaction==null&&!state.busy&&!state.chatWorking) {
                    model.readingActivity(true,true)
                    while(isActive){delay(1000);model.readingActivity(true)}
                } else awaitCancellation()
            } finally {withContext(NonCancellable){model.readingActivity(false)}}
        }
    }
    TortoTheme(state) {
        val route=routes.lastOrNull()
        val title=when(route){
            "settings"->tr("设置","Settings");"system"->tr("系统","System");"typography"->tr("排版","Typesetting")
            "layout"->tr("AI 排版","AI layout");"ai"->tr("AI 提供商","AI providers");"translation"->tr("翻译","Translation");"assistant"->tr("阅读助手","Reading assistant")
            "sync"->tr("云同步","Cloud sync");"about"->tr("关于 Torto","About Torto");"statistics"->tr("阅读统计","Reading statistics");"book-statistics"->tr("阅读详情","Reading details");"licenses"->tr("开源许可","Open-source licenses")
            else->if(tab==0)tr("书架","Library") else tr("我","Me")
        }
        Scaffold(
            containerColor=MaterialTheme.colorScheme.background,
            snackbarHost={SnackbarHost(snackbar)},
            topBar={if(state.active==null||state.chromeVisible)TopAppBar(
                title={if(state.active==null)Text(title)},
                navigationIcon={if(state.active!=null||route!=null)ActionIcon("back",tr("返回","Back")){back()}},
                actions={
                    if(state.active!=null){
                        if(state.processing)ActionIcon("stop",tr("停止处理","Stop processing")){model.cancelAi(true)}
                        ActionIcon("chat",tr("阅读助手","Reading assistant"),!state.busy){model.loadChat();panel="chat"}
                        ActionIcon("search",tr("搜索","Search"),!state.chatWorking){panel="search"}
                    } else if(route==null&&tab==0){
                        if(state.cloud?.enabled==true)ActionIcon("sync",tr("立即同步","Sync now"),!state.busy){model.synchronize()}
                        ActionIcon("add",tr("导入书籍","Import book"),!state.busy){picker.launch(arrayOf("*/*"))}
                    }
                },colors=TopAppBarDefaults.topAppBarColors(containerColor=MaterialTheme.colorScheme.background)
            )},
            bottomBar={
                if(state.active!=null&&state.chromeVisible) Column(Modifier.navigationBarsPadding().background(MaterialTheme.colorScheme.surface)) {
                    Row(Modifier.fillMaxWidth().height(56.dp),horizontalArrangement=Arrangement.SpaceEvenly,verticalAlignment=Alignment.CenterVertically){
                        ActionIcon("toc",tr("目录","Contents")){panel="toc"}
                        ActionIcon("marks",tr("批注与高亮","Notes and highlights"),!state.chatWorking){model.loadAnnotations();panel="marks"}
                        IconButton(onClick={model.translate(!state.translated)},enabled=!state.busy&&!state.chatWorking){AppIcon("translate",tr("翻译","Translation"),tint=if(state.translated)MaterialTheme.colorScheme.primary else LocalContentColor.current)}
                        ActionIcon("select",tr("文字选择模式","Selection mode"),!state.translated){panel="selection"}
                        ActionIcon("type",tr("字体排版","Typography")){panel="typography"}
                        ActionIcon(if(state.dark)"sun" else "moon",tr("切换明暗模式","Toggle light/dark"),!state.busy&&!state.chatWorking){model.configure(state.fontSize,state.unified,!state.dark)}
                    }
                } else if(state.active==null&&route==null) NavigationBar(containerColor=MaterialTheme.colorScheme.background){
                    NavigationBarItem(selected=tab==0,onClick={tab=0},icon={AppIcon("library")},label={Text(tr("书架","Library"))})
                    NavigationBarItem(selected=tab==1,onClick={tab=1;model.loadStatistics()},icon={AppIcon("person")},label={Text(tr("我","Me"))})
                }
            }
        ){padding->
            Box(Modifier.fillMaxSize().padding(padding)) {
                if(state.active!=null) Column(Modifier.fillMaxSize()) {
                    Box(Modifier.fillMaxWidth().weight(1f)){
                    AndroidView(factory={ReaderView(it).apply {onSize=model::measure;onTap=model::tap;onTurn={next->if(!model.state.value.chatWorking)model.turn(next)};onScroll=model::scroll;onHandleStart=model::beginHandle;onHandleMove=model::dragHandle;onHandleEnd=model::endHandle;onSelect={x,y,ex,ey->if(!model.state.value.chatWorking)model.select(x,y,ex,ey)}}},update={it.present(state.frame,state.selected)},modifier=Modifier.fillMaxSize())
                    if(state.processing||state.aiError!=null||state.aiPaused)Column(Modifier.align(Alignment.TopCenter).fillMaxWidth().background(MaterialTheme.colorScheme.surfaceContainerLow).padding(horizontal=16.dp,vertical=8.dp)){
                        if(state.processing){
                            val stage=when(state.aiStage){"translation"->tr("正在翻译","Translating");"layout"->tr("正在排版","Recognizing layout");"toc"->tr("正在翻译目录","Translating contents");"prefetch"->tr("正在预处理下一页","Processing next page");else->tr("准备处理","Preparing")}
                            Text(stage+(if(state.aiTotal>0u)" ${state.aiCompleted}/${state.aiTotal}" else ""),style=MaterialTheme.typography.labelMedium)
                            if(state.aiTotal>0u)LinearProgressIndicator(progress={state.aiCompleted.toFloat()/state.aiTotal.toFloat()},modifier=Modifier.fillMaxWidth()) else LinearProgressIndicator(Modifier.fillMaxWidth())
                        }else{Text(state.aiError ?: tr("AI 处理已暂停","AI processing paused"),maxLines=3,overflow=androidx.compose.ui.text.style.TextOverflow.Ellipsis,color=if(state.aiError!=null)MaterialTheme.colorScheme.error else LocalContentColor.current)
                            Row{TextButton(onClick=model::retryAi){Text(tr("继续／重试","Resume / retry"))};TextButton(onClick={model.loadServices();panel=if(state.translated)"translation-settings" else "layout-settings"}){Text(tr("检查设置","Check settings"))}}
                        }
                    }
                    }
                    val page=state.frame?.info
                    Text(page?.let {"${it.section+1u}/${it.sectionCount} · ${(it.progress*100).toInt()}%"+(if(state.focus&&it.focusCount>0u)" · ${it.focusIndex+1u}/${it.focusCount}" else "")} ?: "",Modifier.align(Alignment.CenterHorizontally).padding(vertical=4.dp),style=MaterialTheme.typography.labelSmall,color=MaterialTheme.colorScheme.onSurfaceVariant)
                } else when(route){
                    "settings"->SettingsScreen(::open)
                    "system"->SystemScreen(state,model)
                    "typography"->TypographyPanel(state,model,openLayout={open("layout")},openTranslation={open("translation")})
                    "ai","translation","assistant","sync","layout"->ServiceSettingsPanel(state,model,route){open("ai")}
                    "statistics"->StatisticsScreen(state,model){id->model.selectStatisticsBook(id);open("book-statistics")}
                    "book-statistics"->BookStatisticsScreen(state,model)
                    "about"->AboutScreen(state,model){open("licenses")}
                    "licenses"->LicenseScreen()
                    else->if(tab==0)ShelfScreen(state,model){bookActions=it} else ProfileScreen(state,{open("settings")},{open("statistics")})
                }
                    state.selected?.let {selection->
                        Row(Modifier.align(Alignment.BottomCenter).fillMaxWidth().horizontalScroll(rememberScrollState()).background(MaterialTheme.colorScheme.secondaryContainer).padding(horizontal=12.dp),verticalAlignment=Alignment.CenterVertically){
                            TextButton(onClick={model.saveAnnotation(null)}){Text(tr("高亮","Highlight"))}
                            TextButton(onClick={editing=null;panel="note"}){Text(tr("批注","Note"))}
                            TextButton(onClick={context.getSystemService(ClipboardManager::class.java).setPrimaryClip(ClipData.newPlainText("",selection.text));model.clearSelection()}){Text(tr("复制","Copy"))}
                            TextButton(onClick={model.loadChat();panel="chat"}){Text(tr("提问","Ask"))}
                            ActionIcon("close",tr("取消选择","Clear selection")){model.clearSelection()}
                        }
                    }

                if(state.busy)LinearProgressIndicator(Modifier.fillMaxWidth().align(Alignment.TopCenter))
            }
        }
        state.interaction?.let {interaction->
            if(interaction.kind in listOf("image","formula"))MediaPreview(interaction,model::dismissInteraction,if(interaction.kind=="image"&&interaction.target.isNotEmpty())({model.attachBookImages(interaction.target);model.dismissInteraction();model.loadChat();panel="chat"})else null)
            else if(interaction.kind=="note")ModalBottomSheet(onDismissRequest=model::dismissInteraction){
                Column(Modifier.fillMaxWidth().heightIn(max=580.dp).verticalScroll(rememberScrollState()).padding(24.dp).navigationBarsPadding(),verticalArrangement=Arrangement.spacedBy(16.dp)){
                    Text(tr("注释","Note"),style=MaterialTheme.typography.titleLarge)
                    val assets=remember(interaction){AssistantAssetCache()}
                    AssistantRichText(interaction.markup.ifBlank {interaction.text},assets){target->
                        val uri=Uri.parse(target)
                        if(uri.scheme in listOf("https","http"))context.startActivity(Intent(Intent.ACTION_VIEW,uri))
                        else {model.followLink(target);model.dismissInteraction()}
                    }
                    if(interaction.target.isNotEmpty())TextButton(onClick={model.followLink(interaction.target);model.dismissInteraction()}){Text(tr("查看原文位置","Go to original note"))}
                }
            }
        }
        if(panel!=null)ModalBottomSheet(onDismissRequest={closePanel()},sheetState=rememberModalBottomSheetState(skipPartiallyExpanded=panel=="chat")) {
            when(panel){
                "toc"->LazyColumn(Modifier.fillMaxHeight(.8f),contentPadding=PaddingValues(16.dp)){
                    item{Text(state.active?.title.orEmpty(),style=MaterialTheme.typography.titleLarge,modifier=Modifier.padding(bottom=16.dp))}
                    items(state.toc){entry->ListItem(headlineContent={Text(entry.title)},modifier=Modifier.padding(start=(entry.depth.toInt()*16).dp).clickable {model.navigate(entry.href);closePanel()},colors=ListItemDefaults.colors(containerColor=MaterialTheme.colorScheme.surfaceContainerLow))}
                }
                "search"->Column(Modifier.fillMaxHeight(.85f).padding(20.dp).imePadding()){
                    OutlinedTextField(query,{query=it},label={Text(tr("搜索书中内容","Search in book"))},singleLine=true,modifier=Modifier.fillMaxWidth(),trailingIcon={ActionIcon("search",tr("搜索","Search"),!state.busy){model.search(query)}})
                    LazyColumn{items(state.hits){hit->ListItem(headlineContent={Text(hit.title,style=MaterialTheme.typography.labelMedium)},supportingContent={Text(hit.excerpt)},modifier=Modifier.clickable {model.jump(hit);closePanel()},colors=ListItemDefaults.colors(containerColor=MaterialTheme.colorScheme.surfaceContainerLow))}}
                }
                "typography"->TypographyPanel(state,model,::closePanel,{panel="layout-settings";model.loadServices()},{panel="translation-settings";model.loadServices()})
                "layout-settings"->ServiceSettingsPanel(state,model,"layout"){panel="providers"}
                "translation-settings"->ServiceSettingsPanel(state,model,"translation"){panel="providers"}
                "providers"->ProviderManager(state,model)
                "marks"->AnnotationsPanel(state,model,{editing=it;panel="note"},::closePanel)
                "note"->NoteEditor(editing?.quote ?: state.selected?.text.orEmpty(),editing?.note){text->editing?.let {model.editAnnotation(it.id,text)} ?: model.saveAnnotation(text);closePanel()}
                "chat"->ChatPanel(state,model,::closePanel)
                "selection"->Column(Modifier.navigationBarsPadding()){
                    for((mode,label) in listOf("free" to tr("自由","Free"),"word" to tr("单词","Word"),"sentence" to tr("句子","Sentence"),"paragraph" to tr("段落","Paragraph"))) ListItem(headlineContent={Text(label)},trailingContent={if(state.selectionMode==mode)AppIcon("check")},modifier=Modifier.clickable {model.selectionMode(mode);closePanel()})
                }
            }
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

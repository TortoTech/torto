package org.tortotech.torto

import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.*
import androidx.compose.foundation.interaction.collectIsDraggedAsState
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.saveable.mapSaver
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.platform.*
import androidx.compose.ui.semantics.*
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.conflate
import kotlinx.coroutines.launch
import org.tortotech.torto.core.ChatMessage

@Stable internal class AssistantUiState(index:Int=0,offset:Int=0,following:Boolean=true,thoughts:Map<String,Boolean> = emptyMap(),references:Map<String,Boolean> = emptyMap()){
    val list=LazyListState(index,offset)
    var follow by mutableStateOf(following)
    val reasoning=mutableStateMapOf<String,Boolean>().apply {putAll(thoughts)}
    val sources=mutableStateMapOf<String,Boolean>().apply {putAll(references)}
    companion object {
        val Saver=mapSaver(save={mapOf("index" to it.list.firstVisibleItemIndex,"offset" to it.list.firstVisibleItemScrollOffset,"follow" to it.follow,"thoughts" to HashMap(it.reasoning),"sources" to HashMap(it.sources))},restore={
            @Suppress("UNCHECKED_CAST")
            AssistantUiState(it["index"] as Int,it["offset"] as Int,it["follow"] as Boolean,it["thoughts"] as Map<String,Boolean>,it["sources"] as Map<String,Boolean>)
        })
    }
}
@Composable internal fun rememberAssistantUiState(book:String?)=rememberSaveable(book,saver=AssistantUiState.Saver){AssistantUiState()}

/** Full-screen reader overlay. Its saved state is owned by the book, outside this window. */
@Composable internal fun ReadingAssistantDialog(state:ScreenState,model:ReaderModel,close:()->Unit,ui:AssistantUiState=rememberAssistantUiState(state.active?.id)){
    FullScreenInputDialog(close,state.readingBrightness){dismiss->ChatPanel(state,model,dismiss,ui)}
}

@OptIn(ExperimentalLayoutApi::class)
@Composable internal fun ChatPanel(state:ScreenState,model:ReaderModel,close:()->Unit,ui:AssistantUiState){
    val context=LocalContext.current
    val density=LocalDensity.current
    val inputFocus=rememberInputFocus()
    val assets=remember(state.active?.id){AssistantAssetCache()}
    val list=ui.list
    val dragged by list.interactionSource.collectIsDraggedAsState()
    val scope=rememberCoroutineScope()
    val latest by rememberUpdatedState(state)
    val link:(String)->Unit={target->
        if(target.startsWith("link://j/")){model.followChatCitation(target);close()}
        else {val uri=Uri.parse(target);if(uri.scheme in listOf("https","http"))runCatching {context.startActivity(Intent(Intent.ACTION_VIEW,uri))}}
    }
    var confirmClear by remember {mutableStateOf(false)}
    var deleteId by remember {mutableStateOf<String?>(null)}
    var menu by remember {mutableStateOf(false)}
    LaunchedEffect(dragged,list.canScrollForward){if(dragged)ui.follow=false else if(!list.canScrollForward)ui.follow=true}
    LaunchedEffect(state.messages,state.chatWorking,state.busy){if(!state.chatWorking&&!state.busy){
        val ids=state.messages.map {it.id}.toSet()
        ui.reasoning.keys.retainAll(ids);ui.sources.keys.retainAll(ids)
    }}
    LaunchedEffect(list){
        snapshotFlow {
            val layout=list.layoutInfo
            listOf(latest.messages.size,latest.chatWorking,latest.chatContent,latest.chatReasoning,latest.chatActivity,ui.follow,dragged,
                layout.viewportEndOffset,layout.totalItemsCount,layout.visibleItemsInfo.lastOrNull()?.let {it.index to it.size})
        }.conflate().collect {
            if(ui.follow&&!dragged){delay(80);if(ui.follow&&!dragged){val count=list.layoutInfo.totalItemsCount;if(count>0)list.scrollToItem(count-1,Int.MAX_VALUE)}}
        }
    }
    val pane=tr("阅读助手","Reading assistant")
    val inputLabel=tr("针对本书提出你的问题","Ask a question about this book")
    val streaming=state.chatWorking&&state.messages.none {it.id==state.chatStreamId}
    val ime=WindowInsets.ime.getBottom(density)>0
    Column(Modifier.fillMaxSize().background(MaterialTheme.colorScheme.background).semantics {paneTitle=pane}.statusBarsPadding().navigationBarsPadding().imePadding()){
        Box(Modifier.fillMaxWidth().height(64.dp)){
            Column(Modifier.align(Alignment.Center).fillMaxWidth().padding(horizontal=56.dp),horizontalAlignment=Alignment.CenterHorizontally){
                Text(pane,fontSize=16.sp,lineHeight=22.sp,color=MaterialTheme.colorScheme.onSurface)
                state.active?.title?.takeIf {it.isNotBlank()}?.let {Text(it,fontSize=12.sp,lineHeight=18.sp,color=MaterialTheme.colorScheme.onSurfaceVariant,maxLines=1,overflow=TextOverflow.Ellipsis)}
            }
            ActionIcon("expand",tr("收起助手","Collapse assistant"),modifier=Modifier.align(Alignment.CenterStart).padding(start=4.dp),onClick=close)
            Box(Modifier.align(Alignment.CenterEnd)){
                ActionIcon("more",tr("助手菜单","Assistant menu")){menu=true}
                AssistantMenu(menu,{menu=false},above=false){
                    AssistantMenuItem("undo",tr("还原临时改写","Undo rewrites"),!state.chatWorking&&!state.busy&&state.changes.isEmpty()){menu=false;model.undoRewrites()}
                    AssistantMenuItem("clear",tr("清空本次对话","Clear conversation"),!state.chatWorking&&!state.busy&&state.messages.isNotEmpty()){menu=false;confirmClear=true}
                }
            }
        }
        Box(Modifier.weight(1f).fillMaxWidth()){
            LazyColumn(Modifier.fillMaxSize(),state=list,contentPadding=PaddingValues(horizontal=16.dp,vertical=16.dp),verticalArrangement=Arrangement.spacedBy(12.dp)){
                items(state.messages,key={it.id}){message->
                    if(message.user)AssistantQuestion(message.content,message.images.map {it.label to it.id},model::previewChatImage)
                    else AssistantAnswer(message,state,model,assets,ui,link,{deleteId=message.id},clear={confirmClear=true},changes=if(message.id==state.messages.lastOrNull()?.id)state.changes else emptyList())
                }
                if(streaming){
                    item(key="${state.chatStreamId}-question"){AssistantQuestion(state.chatQuestion,emptyList(),model::previewChatImage)}
                    item(key=state.chatStreamId){
                        AssistantAnswer(ChatMessage(id=state.chatStreamId,user=false,content=state.chatContent,images=emptyList(),progress=listOf(state.chatReasoning,state.chatActivity).filter {it.isNotBlank()},thinkingSeconds=null,stopped=false,citations=emptyList()),state,model,assets,ui,link,{},streaming=true)
                    }
                }
            }
            if(!ui.follow&&list.canScrollForward)FilledTonalButton(onClick={scope.launch {val count=list.layoutInfo.totalItemsCount;if(count>0)list.animateScrollToItem(count-1,Int.MAX_VALUE);ui.follow=true}},modifier=Modifier.align(Alignment.BottomCenter).padding(bottom=8.dp)){
                AppIcon("expand",size=18.dp);Spacer(Modifier.width(6.dp));Text(tr("回到底部","Back to bottom"),fontSize=13.sp)
            }
        }
        Column(Modifier.fillMaxWidth().padding(horizontal=16.dp).padding(bottom=12.dp),verticalArrangement=Arrangement.spacedBy(12.dp)){
            if(!ime)Row(Modifier.fillMaxWidth(),horizontalArrangement=Arrangement.spacedBy(6.dp)){
                val prompts=listOf(
                    tr("书籍亮点","Highlights") to tr("请结合全书内容总结本书的主要亮点，并提供原文引源。","Summarize the main highlights of this book using the full book and cite the sources."),
                    tr("背景解读","Background") to tr("请解读本书相关的历史、文化与写作背景，结合书中内容并提供引源。","Explain this book's historical, cultural and writing background, grounded in its content with citations."),
                    tr("关键概念","Key concepts") to tr("请梳理本书的关键概念及它们的联系，并提供原文引源。","Explain this book's key concepts and their relationships, with source citations.")
                )
                for((title,prompt)in prompts)Surface(Modifier.weight(1f),shape=RoundedCornerShape(12.dp),color=MaterialTheme.colorScheme.surfaceContainerLowest,border=BorderStroke(.5.dp,MaterialTheme.colorScheme.outlineVariant.copy(alpha=.5f))){
                    Box(Modifier.heightIn(min=40.dp).clickable(enabled=!state.chatWorking&&!state.busy&&state.changes.isEmpty()){ui.follow=true;model.sendChat(prompt)}.padding(horizontal=4.dp),contentAlignment=Alignment.Center){Text(title,fontSize=13.sp,maxLines=1,overflow=TextOverflow.Ellipsis)}
                }
            }
            val send=state.chatWorking||state.chatDraft.isNotBlank()&&!state.busy&&state.changes.isEmpty()
            val sendLabel=if(state.chatWorking)tr("停止生成","Stop generating")else tr("发送","Send")
            Surface(Modifier.fillMaxWidth(),shape=RoundedCornerShape(20.dp),color=MaterialTheme.colorScheme.surfaceContainerLowest){
                Row(Modifier.fillMaxWidth().heightIn(min=52.dp).padding(start=14.dp,end=8.dp,top=8.dp,bottom=8.dp),verticalAlignment=Alignment.CenterVertically){
                    BasicTextField(state.chatDraft,model::editChatDraft,Modifier.weight(1f).focusRequester(inputFocus).semantics {contentDescription=inputLabel},maxLines=4,
                        textStyle=MaterialTheme.typography.bodyLarge.copy(fontSize=16.sp,color=MaterialTheme.colorScheme.onSurface),cursorBrush=SolidColor(brandControlColor()),
                        decorationBox={input->Box {if(state.chatDraft.isEmpty())Text(tr("针对本书提出你的问题","Ask a question about this book"),fontSize=16.sp,color=MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha=.65f));input()}})
                    Spacer(Modifier.width(8.dp))
                    Box(Modifier.size(40.dp).clip(CircleShape).background(brandControlColor().copy(alpha=if(send)1f else .3f)).semantics {contentDescription=sendLabel}.clickable(enabled=send,role=Role.Button){if(state.chatWorking)model.cancelChat()else{ui.follow=true;model.sendChat(state.chatDraft)}},contentAlignment=Alignment.Center){
                        if(state.chatWorking)Box(Modifier.size(12.dp).background(Color.White,RoundedCornerShape(2.dp)))else AppIcon("up",size=22.dp,tint=Color.White)
                    }
                }
            }
        }
    }
    if(confirmClear)AlertDialog(onDismissRequest={confirmClear=false},title={Text(tr("清空本次对话？","Clear conversation?"))},confirmButton={TextButton(onClick={model.clearChat();confirmClear=false}){Text(tr("清空","Clear"))}},dismissButton={TextButton(onClick={confirmClear=false}){Text(tr("取消","Cancel"))}})
    if(deleteId!=null)AlertDialog(onDismissRequest={deleteId=null},title={Text(tr("删除这一轮问答？","Delete this question and answer?"))},confirmButton={TextButton(onClick={deleteId?.let(model::deleteChatTurn);deleteId=null}){Text(tr("删除","Delete"))}},dismissButton={TextButton(onClick={deleteId=null}){Text(tr("取消","Cancel"))}})
}

@Composable private fun AssistantQuestion(text:String,images:List<Pair<String,String>>,preview:(String)->Unit){
    BoxWithConstraints(Modifier.fillMaxWidth(),contentAlignment=Alignment.CenterEnd){
        Surface(Modifier.widthIn(max=minOf(maxWidth*.88f,520.dp)),shape=RoundedCornerShape(16.dp,16.dp,4.dp,16.dp),color=MaterialTheme.colorScheme.primary.copy(alpha=.1f),contentColor=MaterialTheme.colorScheme.onSurface){
            Column(Modifier.padding(14.dp),verticalArrangement=Arrangement.spacedBy(8.dp)){
                if(text.isNotEmpty())Text(text,fontSize=16.sp,lineHeight=24.sp)
                for((label,id)in images)TextButton(onClick={preview(id)}){AppIcon("image",size=18.dp);Spacer(Modifier.width(6.dp));Text(label,maxLines=1,overflow=TextOverflow.Ellipsis)}
            }
        }
    }
}

private fun Modifier.assistantRule(color:Color)=drawBehind {drawLine(color,Offset(1.dp.toPx(),0f),Offset(1.dp.toPx(),size.height),2.dp.toPx())}

@Composable private fun AssistantAnswer(message:ChatMessage,state:ScreenState,model:ReaderModel,assets:AssistantAssetCache,ui:AssistantUiState,link:(String)->Unit,delete:()->Unit,clear:()->Unit={},streaming:Boolean=false,changes:List<org.tortotech.torto.core.ChatChange> = emptyList()){
    var menu by remember {mutableStateOf(false)}
    val expanded=ui.reasoning[message.id]?:streaming
    val sources=ui.sources[message.id]==true
    LaunchedEffect(sources,message.id){if(sources)model.loadChatSources(message)}
    Surface(Modifier.fillMaxWidth(),shape=RoundedCornerShape(16.dp),color=MaterialTheme.colorScheme.surfaceContainerLowest){
        Column(Modifier.padding(18.dp),verticalArrangement=Arrangement.spacedBy(16.dp)){
            val progress=message.progress.filter {it.isNotBlank()}
            if(progress.isNotEmpty()||streaming&&message.content.isEmpty()){
                Row(Modifier.fillMaxWidth().heightIn(min=32.dp).clickable(enabled=progress.isNotEmpty()){ui.follow=false;ui.reasoning[message.id]=!expanded},verticalAlignment=Alignment.CenterVertically){
                    Text(if(streaming)if(message.content.isEmpty())tr("思考中…","Thinking…")else tr("思考过程","Reasoning")else message.thinkingSeconds?.let {tr("思考过程（用时 ${it} 秒）","Reasoning (${it}s)")}?:tr("思考过程","Reasoning"),fontSize=13.sp,color=MaterialTheme.colorScheme.onSurfaceVariant)
                    if(progress.isNotEmpty())AppIcon("expand",modifier=Modifier.rotate(if(expanded)180f else 0f),size=18.dp)
                }
                if(expanded&&progress.isNotEmpty())Column(Modifier.assistantRule(MaterialTheme.colorScheme.outlineVariant).padding(start=12.dp),verticalArrangement=Arrangement.spacedBy(12.dp)){
                    for(text in progress)Text(text,fontSize=14.sp,lineHeight=22.sp,color=MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
            if(message.content.isNotEmpty())AssistantRichText(message.content,assets,streaming,link)
            if(message.stopped)Text(tr("已停止","Stopped"),fontSize=12.sp,color=MaterialTheme.colorScheme.onSurfaceVariant)
            if(message.citations.isNotEmpty()){
                Row(Modifier.fillMaxWidth().heightIn(min=32.dp).clickable {ui.follow=false;ui.sources[message.id]=!sources},verticalAlignment=Alignment.CenterVertically){
                    Text(tr("书籍引源 · ${message.citations.size}","Book sources · ${message.citations.size}"),fontSize=13.sp,color=MaterialTheme.colorScheme.onSurfaceVariant)
                    AppIcon("expand",modifier=Modifier.rotate(if(sources)180f else 0f),size=18.dp)
                }
                if(sources){
                    when {
                        message.id in state.chatLoadingSources->Text(tr("正在加载引源…","Loading sources…"),fontSize=13.sp,color=MaterialTheme.colorScheme.onSurfaceVariant)
                        message.id in state.chatSourceErrors->TextButton(onClick={model.loadChatSources(message)}){Text(tr("重新加载引源","Retry sources"))}
                        state.chatSources[message.id]?.isEmpty()==true->Text(tr("未找到可定位的引源","No available source locations"),fontSize=13.sp,color=MaterialTheme.colorScheme.onSurfaceVariant)
                        else->for(source in state.chatSources[message.id].orEmpty()){
                            val marker=assistantCitationMarker(message.citations.indexOf(source.locator)+1)
                            val title=remember(source.title){source.title.lineSequence().joinToString(" "){it.trim()}}
                            Column(Modifier.fillMaxWidth().clickable {link(source.locator)},verticalArrangement=Arrangement.spacedBy(6.dp)){
                                Text("$marker $title",fontSize=14.sp,color=MaterialTheme.colorScheme.primary,maxLines=1,overflow=TextOverflow.Ellipsis)
                                if(source.excerpt.isNotBlank()&&source.excerpt.trim()!=source.title.trim())Text(source.excerpt,fontSize=13.sp,lineHeight=20.sp,color=MaterialTheme.colorScheme.onSurfaceVariant,maxLines=3,overflow=TextOverflow.Ellipsis)
                            }
                            HorizontalDivider(color=MaterialTheme.colorScheme.outlineVariant.copy(alpha=.4f),thickness=.5.dp)
                        }
                    }
                }
            }
            if(changes.isNotEmpty()){
                Text(tr("确认助手提出的修改","Review suggested changes"),fontSize=14.sp)
                for(change in changes){Text(change.kind,fontSize=13.sp,color=MaterialTheme.colorScheme.primary);Text(change.text,fontSize=14.sp)}
                Row(horizontalArrangement=Arrangement.spacedBy(12.dp)){
                    Button(onClick={model.resolveChanges(true)},enabled=!state.busy){Text(tr("应用","Apply"))}
                    TextButton(onClick={model.resolveChanges(false)},enabled=!state.busy){Text(tr("放弃","Discard"))}
                }
            }
            if(!streaming)Box(Modifier.fillMaxWidth(),contentAlignment=Alignment.CenterEnd){
                Box {
                    ActionIcon("more",tr("回答菜单","Answer menu"),!state.chatWorking&&!state.busy,iconSize=20.dp){menu=true}
                    AssistantMenu(menu,{menu=false},above=true){
                        AssistantMenuItem("delete",tr("删除","Delete")){menu=false;delete()}
                        AssistantMenuItem("clear",tr("清空本次对话","Clear conversation")){menu=false;clear()}
                    }
                }
            }
        }
    }
}

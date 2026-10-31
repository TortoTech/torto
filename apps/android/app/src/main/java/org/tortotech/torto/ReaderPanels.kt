package org.tortotech.torto

import android.content.Intent
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.ui.platform.LocalContext
import org.tortotech.torto.core.ChatAttachment
import androidx.compose.foundation.*
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import org.tortotech.torto.core.Annotation

@Composable fun TypographyPanel(state:ScreenState,model:ReaderModel,close:()->Unit={},openLayout:()->Unit={},openTranslation:()->Unit={}) {
    var size by remember(state.fontSize) { mutableFloatStateOf(state.fontSize) }
    val options=state.reading
    var weight by remember(options?.fontWeight){mutableFloatStateOf(options?.fontWeight?.toFloat() ?: 400f)}
    LaunchedEffect(Unit){model.loadFonts();model.loadServices()}
    val families=state.fontFamilies
    Column(Modifier.verticalScroll(rememberScrollState()).padding(24.dp).navigationBarsPadding(),verticalArrangement=Arrangement.spacedBy(12.dp)) {
        Text(tr("字体排版","Typography"),style=MaterialTheme.typography.titleLarge)
        Text(tr("字号","Font size")+" ${size.toInt()}")
        Slider(size,{size=it},valueRange=12f..36f,enabled=!state.busy,onValueChangeFinished={model.configure(size,state.unified,state.dark)})
        if(options!=null){
            Text(tr("字重","Font weight")+" ${weight.toInt()}")
            Slider(weight,{weight=it},valueRange=200f..900f,steps=27,enabled=!state.busy,onValueChangeFinished={model.saveReadingOptions(options.copy(fontWeight=weight.toInt().toUShort()))})
            SectionLabel(tr("中日韩书籍","CJK books"))
            SettingChoice(tr("主要字体","Primary font"),options.cjkFont,families?.cjk.orEmpty().map {it to it},!state.busy){model.saveReadingOptions(options.copy(cjkFont=it))}
            SettingChoice(tr("拉丁字体","Latin font"),options.cjkLatinFont,listOf("" to tr("跟随其他语言字体","Use other-language font"))+families?.latin.orEmpty().map {it to it},!state.busy){model.saveReadingOptions(options.copy(cjkLatinFont=it))}
            SectionLabel(tr("其他语言书籍","Other-language books"))
            SettingChoice(tr("主要字体","Primary font"),options.latinFont,families?.latin.orEmpty().map {it to it},!state.busy){model.saveReadingOptions(options.copy(latinFont=it))}
            SettingChoice(tr("中日韩字体","CJK font"),options.latinCjkFont,listOf("" to tr("跟随中日韩主要字体","Use CJK primary font"))+families?.cjk.orEmpty().map {it to it},!state.busy){model.saveReadingOptions(options.copy(latinCjkFont=it))}
            SettingChoice(tr("等宽字体","Monospace font"),options.monospaceFont,families?.monospace.orEmpty().map {it to it},!state.busy){model.saveReadingOptions(options.copy(monospaceFont=it))}
        }
        OutlinedButton(onClick=model::downloadFonts,enabled=!state.busy){Text(tr("下载默认字体","Download reading fonts"))}
        SettingToggle(tr("统一版式","Unified layout"),state.unified,!state.busy){model.configure(size,it,state.dark)}
        if(state.active!=null)SettingToggle(tr("专注模式","Focus mode"),state.focus,!state.busy){model.focus(it)}
        if(options!=null){
            SettingToggle(tr("滚动专注","Scrolling focus"),options.scrollFocus,!state.busy){model.saveReadingOptions(options.copy(scrollFocus=it))}
            SettingToggle(tr("按句分段显示","Split sentences onto separate lines"),options.sentenceSplit,!state.busy){model.saveReadingOptions(options.copy(sentenceSplit=it))}
            SettingChoice(tr("专注样式","Focus style"),options.focusStyle,listOf("dim" to tr("周边淡化","Dim surroundings"),"highlight" to tr("突出当前内容","Highlight current content")),!state.busy){model.saveReadingOptions(options.copy(focusStyle=it))}
        }
        SettingToggle(tr("自动 AI 排版","Automatic AI layout"),state.autoLayout,!state.busy){model.automaticLayout(it)}
        TextButton(onClick=openLayout){Text(tr("AI 排版设置","AI layout settings"))}
        TextButton(onClick=openTranslation){Text(tr("翻译设置","Translation settings"))}
        if(state.active!=null) {
            Button(onClick={model.recognize();close()},enabled=!state.busy){AppIcon("ai");Spacer(Modifier.width(8.dp));Text(tr("AI 排版本页","AI layout for this page"))}
            TextButton(onClick={model.markFinished();close()},enabled=!state.busy){Text(tr("标记为读完","Mark as finished"))}
        }
    }
}
@Composable fun AnnotationsPanel(state:ScreenState,model:ReaderModel,edit:(Annotation)->Unit,close:()->Unit) {
    var deleting by remember { mutableStateOf<Annotation?>(null) }
    Column(Modifier.fillMaxHeight(.8f).padding(top=12.dp)) {
        Text(tr("批注与高亮","Notes and highlights"),Modifier.padding(horizontal=20.dp,vertical=12.dp),style=MaterialTheme.typography.titleLarge)
        if(state.annotations.isEmpty()) Text(tr("长按正文，添加高亮或批注","Long press the text to add a highlight or note"),Modifier.padding(24.dp),color=MaterialTheme.colorScheme.onSurfaceVariant)
        LazyColumn(contentPadding=PaddingValues(16.dp),verticalArrangement=Arrangement.spacedBy(12.dp)) {
            items(state.annotations,key={it.id}) { note ->
                Card(onClick={model.jumpAnnotation(note);close()},colors=CardDefaults.cardColors(containerColor=MaterialTheme.colorScheme.surfaceContainerLow)) {
                    Column(Modifier.padding(16.dp)) {
                        Text(note.quote,maxLines=4,overflow=TextOverflow.Ellipsis)
                        note.note?.let { Spacer(Modifier.height(8.dp));Text(it,color=MaterialTheme.colorScheme.primary) }
                        Row(Modifier.fillMaxWidth(),horizontalArrangement=Arrangement.End) {
                            ActionIcon("edit",tr("编辑批注","Edit note")){edit(note)}
                            ActionIcon("delete",tr("删除批注","Delete annotation")){deleting=note}
                        }
                    }
                }
            }
        }
    }
    deleting?.let { note -> AlertDialog(onDismissRequest={deleting=null},title={Text(tr("删除批注？","Delete annotation?"))},text={Text(note.quote,maxLines=4)},confirmButton={TextButton(onClick={model.deleteAnnotation(note.id);deleting=null}){Text(tr("删除","Delete"))}},dismissButton={TextButton(onClick={deleting=null}){Text(tr("取消","Cancel"))}}) }
}
@Composable fun NoteEditor(quote:String,initial:String?,save:(String)->Unit) {
    var text by remember(quote,initial){mutableStateOf(initial.orEmpty())}
    Column(Modifier.padding(20.dp).imePadding().navigationBarsPadding(),verticalArrangement=Arrangement.spacedBy(12.dp)) {
        Text(quote,maxLines=3,overflow=TextOverflow.Ellipsis)
        OutlinedTextField(text,{text=it},modifier=Modifier.fillMaxWidth(),minLines=3,maxLines=6,placeholder={Text(tr("写下批注","Write a note"))})
        Button(onClick={save(text)},modifier=Modifier.align(Alignment.End)){Text(tr("保存","Save"))}
    }
}
@Composable fun ChatPanel(state:ScreenState,model:ReaderModel,close:()->Unit={}) {
    val question=state.chatDraft
    val context=LocalContext.current
    val assets=remember(state.active?.id){AssistantAssetCache()}
    val picker=rememberLauncherForActivityResult(ActivityResultContracts.OpenMultipleDocuments()){uris->if(uris.isNotEmpty())model.attachImages(uris)}
    val link:(String)->Unit={target->
        if(target.startsWith("link://j/")){model.followChatCitation(target);close()}
        else {val uri=Uri.parse(target);if(uri.scheme in listOf("https","http"))try{context.startActivity(Intent(Intent.ACTION_VIEW,uri))}catch(_:android.content.ActivityNotFoundException){}}
    }

    var confirmClear by remember { mutableStateOf(false) }
    var showReasoning by remember { mutableStateOf(false) }
    val scroll=rememberLazyListState()
    LaunchedEffect(state.messages.size,state.chatWorking){ val last=state.messages.size+(if(state.chatWorking)2 else 0)-1;if(last>=0)scroll.scrollToItem(last) }
    Column(Modifier.fillMaxHeight(.9f).imePadding().navigationBarsPadding()) {
        Row(Modifier.fillMaxWidth().padding(start=20.dp,end=8.dp),verticalAlignment=Alignment.CenterVertically){
            Text(tr("阅读助手","Reading assistant"),Modifier.weight(1f),style=MaterialTheme.typography.titleLarge)
            ActionIcon("undo",tr("还原临时改写","Undo rewrites"),!state.chatWorking&&state.changes.isEmpty()&&!state.busy){model.undoRewrites()}
            ActionIcon("delete",tr("清空对话","Clear conversation"),!state.chatWorking){confirmClear=true}
        }
        if(state.messages.isEmpty()&&!state.chatWorking) Text(tr("提问、总结章节，或结合书中内容展开讨论","Ask a question, summarize a chapter, or discuss the book"),Modifier.padding(20.dp),color=MaterialTheme.colorScheme.onSurfaceVariant)
        LazyColumn(Modifier.weight(1f),state=scroll,contentPadding=PaddingValues(16.dp),verticalArrangement=Arrangement.spacedBy(16.dp)) {
            items(state.messages){message->ChatBubble(message.user,message.content,message.images,assets,false,link,model::previewChatImage)}
            if(state.chatWorking){
                item{ChatBubble(true,state.chatQuestion,state.sendingImages,assets,false,link,model::previewChatImage)}
                item{
                    Column {
                        if(state.chatActivity.isNotEmpty())Text(state.chatActivity,style=MaterialTheme.typography.labelMedium,color=MaterialTheme.colorScheme.primary)
                        if(state.chatReasoning.isNotEmpty()){TextButton(onClick={showReasoning=!showReasoning}){Text(tr("思考过程","Reasoning"))};if(showReasoning)Text(state.chatReasoning,style=MaterialTheme.typography.bodySmall)}
                        if(state.chatContent.isEmpty()) LinearProgressIndicator(Modifier.fillMaxWidth()) else ChatBubble(false,state.chatContent,emptyList(),assets,true,link,model::previewChatImage)
                    }
                }
            }
            if(state.changes.isNotEmpty()) item {
                Card(colors=CardDefaults.cardColors(containerColor=MaterialTheme.colorScheme.secondaryContainer)) {
                    Column(Modifier.padding(16.dp),verticalArrangement=Arrangement.spacedBy(12.dp)) {
                        Text(tr("确认助手提出的修改","Review suggested changes"),style=MaterialTheme.typography.titleMedium)
                        state.changes.forEach {change-> Text(change.kind,style=MaterialTheme.typography.labelLarge);SelectionContainer{Text(change.text)} }
                        Row(horizontalArrangement=Arrangement.spacedBy(12.dp)) {
                            Button(onClick={model.resolveChanges(true)},enabled=!state.busy){Text(tr("应用","Apply"))}
                            TextButton(onClick={model.resolveChanges(false)},enabled=!state.busy){Text(tr("放弃","Discard"))}
                        }
                    }
                }
            }
        }
        if(state.draftImages.isNotEmpty())Row(Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal=12.dp),horizontalArrangement=Arrangement.spacedBy(8.dp)){
            state.draftImages.forEach {image->InputChip(selected=false,onClick={model.previewChatImage(image.id)},label={Text(image.label,maxLines=1,overflow=TextOverflow.Ellipsis,modifier=Modifier.widthIn(max=160.dp))},trailingIcon={ActionIcon("close",tr("移除图片","Remove image"),!state.chatWorking&&!state.busy){model.removeChatImage(image.id)}})}
        }
        Row(Modifier.fillMaxWidth().padding(12.dp),verticalAlignment=Alignment.Bottom) {
            ActionIcon("image",tr("添加图片","Attach images"),!state.chatWorking&&!state.busy&&state.draftImages.size<8){picker.launch(arrayOf("image/*"))}
            OutlinedTextField(question,model::editChatDraft,modifier=Modifier.weight(1f),placeholder={Text(tr("问问这本书…","Ask about this book…"))},maxLines=4,enabled=!state.chatWorking)
            ActionIcon(if(state.chatWorking)"stop" else "send",if(state.chatWorking)tr("停止生成","Stop generating") else tr("发送","Send"),state.chatWorking || ((question.isNotBlank()||state.draftImages.isNotEmpty())&&state.changes.isEmpty()&&!state.busy)) {
                if(state.chatWorking)model.cancelChat() else {model.sendChat(question)}
            }
        }
    }
    if(confirmClear)AlertDialog(onDismissRequest={confirmClear=false},title={Text(tr("清空对话？","Clear conversation?"))},confirmButton={TextButton(onClick={model.clearChat();confirmClear=false}){Text(tr("清空","Clear"))}},dismissButton={TextButton(onClick={confirmClear=false}){Text(tr("取消","Cancel"))}})
}
@Composable private fun ChatBubble(user:Boolean,text:String,images:List<ChatAttachment>,cache:AssistantAssetCache,streaming:Boolean,onLink:(String)->Unit,onImage:(String)->Unit) {
    Row(Modifier.fillMaxWidth(),horizontalArrangement=if(user)Arrangement.End else Arrangement.Start) {
        Surface(color=if(user)MaterialTheme.colorScheme.secondaryContainer else MaterialTheme.colorScheme.surfaceContainerLow,shape=MaterialTheme.shapes.medium,modifier=Modifier.widthIn(max=520.dp)) {
            Column(Modifier.padding(14.dp),verticalArrangement=Arrangement.spacedBy(8.dp)){
                if(text.isNotEmpty())if(user)SelectionContainer {Text(text)} else AssistantRichText(text,cache,streaming,onLink)
                if(images.isNotEmpty())Column {images.forEach {image->TextButton(onClick={onImage(image.id)}){AppIcon("image");Spacer(Modifier.width(8.dp));Text(image.label,maxLines=1,overflow=TextOverflow.Ellipsis)}}}
            }
        }
    }
}

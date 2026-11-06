package org.tortotech.torto

import android.content.ClipData
import android.content.ClipboardManager
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.launch
import org.tortotech.torto.core.SelectedText

private data class DocumentSelection(val page:UInt,val text:SelectedText)
private data class DocumentDrag(val document:DocumentFrame,val x:Float,val y:Float,val ex:Float,val ey:Float,val handle:Boolean?)

/** Only visible native pages retain bitmaps. Notes use reading typography and source-based selection. */
@Composable internal fun ReadingDocument(interaction:ReaderInteraction,model:ReaderModel,firstLineIndent:Float=0f,onBaseline:(Float?)->Unit={}){
    val context=LocalContext.current
    val scope=rememberCoroutineScope()
    val foreground=MaterialTheme.colorScheme.onSurface.toArgb().toUInt()
    val background=MaterialTheme.colorScheme.surfaceContainerLow.toArgb().toUInt()
    val unavailable=tr("无法显示内容","Unable to display content")
    var revision by remember(interaction){mutableIntStateOf(0)}
    var failure by remember(interaction,revision){mutableStateOf<String?>(null)}
    var selected by remember(interaction,revision){mutableStateOf<DocumentSelection?>(null)}
    LaunchedEffect(selected!=null){model.documentInteraction(selected=selected!=null)}
    DisposableEffect(interaction){onDispose {model.documentInteraction(moving=false,selected=false)}}
    val drags=remember(interaction,revision){Channel<DocumentDrag>(Channel.CONFLATED)}
    DisposableEffect(drags){onDispose {drags.close()}}
    LaunchedEffect(drags){for(drag in drags){
        try {val text=model.documentSelect(drag.document,drag.x,drag.y,drag.ex,drag.ey,drag.handle)
            selected=text?.let {DocumentSelection(drag.document.index,it)}
        }catch(e:CancellationException){throw e}catch(e:Exception){failure=e.message}
    }}
    Column(Modifier.fillMaxWidth()){
        failure?.let {Text(it,color=MaterialTheme.colorScheme.error)}
        BoxWithConstraints(Modifier.fillMaxWidth().heightIn(min=56.dp,max=520.dp)){
            val density=LocalDensity.current.density
            val width=maxWidth.value.toUInt().coerceAtLeast(100u)
            val first by produceState<DocumentFrame?>(null,interaction,width,density,revision,foreground,background,firstLineIndent){
                try {value=model.documentPage(interaction,0u,width,density,foreground,background,firstLineIndent).also {model.processVisible()}}
                catch(e:CancellationException){throw e}catch(e:Exception){failure=e.message ?: unavailable}
            }
            val initial=first
            if(initial==null){if(failure==null)LinearProgressIndicator(Modifier.fillMaxWidth());return@BoxWithConstraints}
            LaunchedEffect(initial.id,initial.firstTextBaseline){onBaseline(initial.firstTextBaseline ?: 0f)}
            val list=rememberLazyListState()
            val naturalHeight=if(initial.total==1u)(initial.frame.bitmap.height/density+(if(initial.controls.isEmpty())0 else 48)).dp else maxHeight
            LazyColumn(Modifier.fillMaxWidth().height(naturalHeight.coerceIn(1.dp,maxHeight)),state=list){
                item {
                    if(initial.controls.isNotEmpty())Row(Modifier.fillMaxWidth()){
                        initial.controls.forEach {control->TextButton(onClick={scope.launch {
                            try {if(model.toggleDocumentControl(initial,control))revision++}
                            catch(e:CancellationException){throw e}catch(e:Exception){failure=e.message}
                        }}){Text(if(control.kind=="formula")if(control.active)tr("收起公式备注","Hide formula notes")else tr("公式备注","Formula notes")
                            else if(control.active)tr("显示表格","Show table")else tr("显示图表","Show chart"))}}
                    }
                }
                items(initial.total.toInt(),key={"${initial.id}:$revision:$it"}){index->
                    val page by produceState(if(index==0)initial else null,initial.id,index,width,density,revision,foreground,background,firstLineIndent){
                        if(index!=0)try {value=model.documentPage(interaction,index.toUInt(),width,density,foreground,background,firstLineIndent)}
                        catch(e:CancellationException){throw e}catch(e:Exception){failure=e.message}
                    }
                    page?.let {document->
                        var handle by remember(document){mutableStateOf<Boolean?>(null)}
                        Box(Modifier.fillMaxWidth().aspectRatio(document.frame.bitmap.width.toFloat()/document.frame.bitmap.height)) {
                        AndroidView(factory={ReaderView(it)},update={view->
                            view.onMotion={model.documentInteraction(moving=it)}
                            view.onTap={x,y->if(selected!=null)selected=null else model.documentTap(document,x,y)}
                            view.onSelect={x,y,ex,ey->drags.trySend(DocumentDrag(document,x,y,ex,ey,null))}
                            view.onHandleStart={handle=it}
                            view.onHandleMove={x,y->drags.trySend(DocumentDrag(document,x,y,x,y,handle))}
                            view.onHandleEnd={handle=null}
                            view.onTurn={forward->scope.launch {list.animateScrollToItem((index+1+if(forward)1 else -1).coerceIn(1,initial.total.toInt()))}}
                            view.present(document.frame,selected?.takeIf {it.page==document.index}?.text)
                        },modifier=Modifier.fillMaxSize())
                        selected?.takeIf {it.page==document.index&&handle==null}?.let {selection->
                            SelectionToolbar(selection.text,
                                onCopy={context.getSystemService(ClipboardManager::class.java).setPrimaryClip(ClipData.newPlainText("",selection.text.text))},
                                onDismiss={selected=null},outsideClip=true)
                        }
                        }
                    } ?: Spacer(Modifier.fillMaxWidth().height(320.dp))
                }
            }
        }
    }
}

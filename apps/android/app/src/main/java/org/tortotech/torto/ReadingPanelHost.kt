package org.tortotech.torto

import androidx.compose.animation.*
import androidx.compose.animation.core.animate
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.*
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveableStateHolder
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.RectangleShape
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.nestedscroll.*
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.unit.Velocity
import androidx.compose.ui.semantics.*
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.text.style.TextAlign
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import kotlin.math.roundToInt

internal fun readingPanelOwner(panel:String?):String?=when(panel){
    "layout-settings","translation-settings"->"typography"
    "note","marks-search"->"marks"
    else->panel
}
internal fun readingBottomPanel(panel:String?)=panel!=null&&panel!="chat"

/** Lives in the reading window, above the book and below the independent footer. */
@Composable internal fun BoxScope.ReadingPanelHost(book:String?,panel:String?,onClose:()->Unit,content:@Composable (String)->Unit){
    key(book){
        val open=readingBottomPanel(panel)
        var previous by remember {mutableStateOf("navigation")}
        if(open)SideEffect {previous=panel!!}
        val displayed=if(open)panel!! else previous
        val safe=readerSafePadding()
        val density=LocalDensity.current
        val footer=WindowInsets(bottom=with(density){(safe.calculateBottomPadding()+ReaderBarHeight).roundToPx()})
        val holder=rememberSaveableStateHolder()
        val opacity=remember(displayed){Animatable(0f)}
        LaunchedEffect(displayed,open){if(open)opacity.animateTo(1f,tween(120))}
        var drag by remember {mutableFloatStateOf(0f)}
        var settle by remember {mutableStateOf<Job?>(null)}
        val scope=rememberCoroutineScope()
        LaunchedEffect(panel){settle?.cancel();drag=0f}
        AnimatedVisibility(open,enter=fadeIn(tween(160))+slideInVertically(tween(200)){it/10},exit=fadeOut(tween(140))+slideOutVertically(tween(180)){it/10},modifier=Modifier.fillMaxSize().windowInsetsPadding(footer).clipToBounds()){
            BoxWithConstraints(Modifier.fillMaxSize()){
                val full=displayed in setOf("navigation","marks","marks-search","layout-settings","translation-settings")
                val limit=if(full)maxHeight else maxHeight*.88f
                val closeLabel=tr("关闭阅读面板","Close reading panel")
                if(!full)Box(Modifier.fillMaxSize().background(Color.Black.copy(alpha=.12f)).clickable(interactionSource=remember {MutableInteractionSource()},indication=null,onClickLabel=closeLabel,onClick=onClose))
                val paneLabel=when(displayed){"navigation"->tr("目录与搜索","Contents and search");"marks"->tr("批注与高亮面板","Notes and highlights panel");"marks-search"->tr("搜索笔记面板","Search notes panel");"typography"->tr("字体排版面板","Typography panel");"selection"->tr("选择模式面板","Selection mode panel");else->tr("阅读设置面板","Reading settings panel")}
                fun finishDrag(velocity:Float){
                    settle?.cancel()
                    if(drag>with(density){limit.toPx()}*.18f || drag>with(density){12.dp.toPx()}&&velocity>900f)onClose()
                    else settle=scope.launch {animate(drag,0f,animationSpec=tween(160)){value,_->drag=value}}
                }
                val finish by rememberUpdatedState<(Float)->Unit>({finishDrag(it)})
                val nested=remember {object:NestedScrollConnection {
                    override fun onPreScroll(available:Offset,source:NestedScrollSource):Offset {
                        if(source!=NestedScrollSource.UserInput||drag<=0f||available.y>=0f)return Offset.Zero
                        settle?.cancel()
                        val consumed=available.y.coerceAtLeast(-drag);drag+=consumed
                        return Offset(0f,consumed)
                    }
                    override fun onPostScroll(consumed:Offset,available:Offset,source:NestedScrollSource):Offset {
                        // A fling reaching the list edge must never drag/dismiss
                        // the sheet. Only remaining finger movement may do so.
                        if(source!=NestedScrollSource.UserInput||available.y<=0f)return Offset.Zero
                        settle?.cancel();drag+=available.y
                        return Offset(0f,available.y)
                    }
                    override suspend fun onPreFling(available:Velocity):Velocity {
                        if(drag<=0f)return Velocity.Zero
                        finish(available.y)
                        return Velocity(0f,available.y)
                    }
                }}
                val bodyDrag=Modifier.nestedScroll(nested).draggable(rememberDraggableState {delta->
                    drag=(drag+delta).coerceAtLeast(0f)
                },Orientation.Vertical,onDragStarted={settle?.cancel()},onDragStopped={finishDrag(it)})
                Surface(Modifier.align(Alignment.BottomCenter).fillMaxWidth().animateContentSize(tween(180),alignment=Alignment.BottomCenter).then(if(full)Modifier.fillMaxHeight()else Modifier.heightIn(max=limit)).offset{IntOffset(0,drag.roundToInt())}.semantics {paneTitle=paneLabel}.then(bodyDrag).pointerInput(Unit){detectTapGestures {}},
                    shape=if(full)RectangleShape else RoundedCornerShape(topStart=20.dp,topEnd=20.dp),color=MaterialTheme.colorScheme.surfaceContainerLow){
                    Column(Modifier.fillMaxWidth().then(if(full)Modifier.fillMaxHeight()else Modifier).imePadding()){
                        if(full)Spacer(Modifier.height(safe.calculateTopPadding()+24.dp))
                        Box(Modifier.fillMaxWidth().height(28.dp),contentAlignment=Alignment.Center){Box(Modifier.size(32.dp,3.dp).background(MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha=.22f),RoundedCornerShape(2.dp)))}
                        Box(Modifier.weight(1f,fill=full).fillMaxWidth().graphicsLayer {alpha=opacity.value}){
                            holder.SaveableStateProvider(displayed){content(displayed)}
                        }
                    }
                }
            }
        }
    }
}

@Composable internal fun ReadingPanelPage(title:String,onBack:()->Unit,content:@Composable ()->Unit){
    Column(Modifier.fillMaxSize()){
        Box(Modifier.fillMaxWidth().heightIn(min=48.dp).padding(horizontal=8.dp)){
            Text(title,Modifier.fillMaxWidth().padding(horizontal=48.dp).align(Alignment.Center),fontSize=16.sp,textAlign=TextAlign.Center)
            ActionIcon("back",tr("返回字体排版","Back to typography"),modifier=Modifier.align(Alignment.CenterStart),onClick=onBack)
        }
        Box(Modifier.weight(1f)){content()}
    }
}

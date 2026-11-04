package org.tortotech.torto

import androidx.compose.animation.*
import androidx.compose.animation.core.animate
import androidx.compose.animation.core.tween
import androidx.compose.foundation.*
import androidx.compose.foundation.gestures.Orientation
import androidx.compose.foundation.gestures.draggable
import androidx.compose.foundation.gestures.rememberDraggableState
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.semantics.*
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import org.tortotech.torto.core.TocItem
import kotlin.math.roundToInt

internal fun visibleContents(items:List<TocItem>,collapsed:Set<String>)=items.filter {entry->entry.ancestors.none {it in collapsed}}

/** Reader-local drawer keeps the footer available; its state survives tab/visibility changes. */
@Composable internal fun BoxScope.ReadingNavigation(state:ScreenState,open:Boolean,model:ReaderModel,onClose:()->Unit){
    val book=state.active?.id
    var search by rememberSaveable(book){mutableStateOf(false)}
    var query by rememberSaveable(book){mutableStateOf("")}
    var submitted by rememberSaveable(book){mutableStateOf(false)}
    var collapsed by remember(book){mutableStateOf<Set<String>>(emptySet())}
    var initialized by remember(book){mutableStateOf(false)}
    val contentsScroll=rememberLazyListState()
    val searchScroll=rememberLazyListState()
    val current=state.frame?.info?.activeTocId
    val ancestors=state.frame?.info?.activeTocPath.orEmpty()
    val visible=remember(state.toc,collapsed){visibleContents(state.toc,collapsed)}
    val keyboard=LocalSoftwareKeyboardController.current
    val focus=remember {FocusRequester()}
    var drag by remember {mutableFloatStateOf(0f)}
    var settle by remember {mutableStateOf<Job?>(null)}
    val scope=rememberCoroutineScope()
    val safe=readerSafePadding()
    val paneLabel=tr("目录与搜索","Contents and search")
    val closedLabel=tr("关闭目录与搜索","Close contents and search")
    LaunchedEffect(open,book,state.toc.isNotEmpty(),current){
        if(open){
            drag=0f
            collapsed=(if(initialized)collapsed else state.toc.filter {it.hasChildren}.map {it.id}.toSet())-ancestors.toSet()
            initialized=true
            val at=visibleContents(state.toc,collapsed).indexOfFirst {it.id==current}
            if(!search&&at>=0){withFrameNanos {};contentsScroll.scrollToItem(at)}
        }
    }
    LaunchedEffect(open){if(!open)keyboard?.hide()}
    LaunchedEffect(open,search){if(open&&search){withFrameNanos {};focus.requestFocus()}}
    AnimatedVisibility(visible=open,enter=fadeIn(tween(180)),exit=fadeOut(tween(180)),modifier=Modifier.fillMaxSize().padding(bottom=safe.calculateBottomPadding()+ReaderBarHeight)){
        Box(Modifier.fillMaxSize().background(Color.Black.copy(alpha=.20f)).clickable(onClickLabel=closedLabel,interactionSource=remember {androidx.compose.foundation.interaction.MutableInteractionSource()},indication=null,onClick=onClose))
    }
    AnimatedVisibility(visible=open,enter=slideInVertically(tween(220)){it}+fadeIn(tween(180)),exit=slideOutVertically(tween(180)){it}+fadeOut(tween(150)),modifier=Modifier.align(Alignment.BottomCenter).padding(bottom=safe.calculateBottomPadding()+ReaderBarHeight).clipToBounds()){
        BoxWithConstraints(Modifier.fillMaxWidth().imePadding()){
            val height=(maxHeight-safe.calculateTopPadding()-12.dp).coerceAtLeast(180.dp)*.92f
            val density=LocalDensity.current.density
            Surface(Modifier.fillMaxWidth().height(height).offset{IntOffset(0,drag.roundToInt())}.semantics {paneTitle=paneLabel},shape=RoundedCornerShape(topStart=28.dp,topEnd=28.dp),color=MaterialTheme.colorScheme.surfaceContainerLow,shadowElevation=8.dp){
                Column {
                    Box(Modifier.fillMaxWidth().height(36.dp).draggable(rememberDraggableState {delta->drag=(drag+delta).coerceAtLeast(0f)},Orientation.Vertical,
                        onDragStarted={settle?.cancel()},onDragStopped={velocity->
                            if(drag>height.value*density*.18f||velocity>900f)onClose()
                            else settle=scope.launch {animate(drag,0f,animationSpec=tween(180)){value,_->drag=value}}
                        }),contentAlignment=Alignment.Center){Box(Modifier.size(32.dp,4.dp).background(MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha=.25f),RoundedCornerShape(2.dp)))}
                    Row(Modifier.padding(horizontal=16.dp).fillMaxWidth().height(56.dp).background(MaterialTheme.colorScheme.surfaceContainerHigh,RoundedCornerShape(24.dp)).padding(4.dp)){
                        listOf(tr("搜本书","Search book"),tr("目录","Contents")).forEachIndexed {index,label->
                            val selected=search==(index==0)
                            Row(Modifier.weight(1f).fillMaxHeight().clip(androidx.compose.foundation.shape.RoundedCornerShape(20.dp)).background(if(selected)MaterialTheme.colorScheme.surface else Color.Transparent,RoundedCornerShape(20.dp)).semantics {this.selected=selected}.clickable(role=Role.Tab){search=index==0},horizontalArrangement=Arrangement.Center,verticalAlignment=Alignment.CenterVertically){
                                if(index==0){AppIcon("search",size=19.dp,tint=MaterialTheme.colorScheme.onSurfaceVariant);Spacer(Modifier.width(6.dp))}
                                Text(label,style=MaterialTheme.typography.labelLarge,color=if(selected)MaterialTheme.colorScheme.onSurface else MaterialTheme.colorScheme.onSurfaceVariant)
                            }
                        }
                    }
                    if(search){
                        fun submit(){if(query.isNotBlank()){submitted=true;keyboard?.hide();model.search(query.trim())}}
                        OutlinedTextField(query,{query=it;submitted=false},Modifier.fillMaxWidth().padding(16.dp).focusRequester(focus),singleLine=true,placeholder={Text(tr("输入本书关键词","Search this book"))},shape=RoundedCornerShape(16.dp),keyboardOptions=KeyboardOptions(imeAction=ImeAction.Search),keyboardActions=KeyboardActions(onSearch={submit()}),trailingIcon={ActionIcon("search",tr("搜索","Search"),query.isNotBlank()&&!state.busy){submit()}})
                        if(submitted){
                            Text(if(state.busy)tr("正在搜索…","Searching…")else tr("找到 ${state.hits.size} 处结果","${state.hits.size} results"),Modifier.padding(horizontal=20.dp,vertical=4.dp),style=MaterialTheme.typography.labelMedium,color=MaterialTheme.colorScheme.onSurfaceVariant)
                            LazyColumn(state=searchScroll,modifier=Modifier.weight(1f),contentPadding=PaddingValues(bottom=16.dp)){
                                items(state.hits){hit->
                                    Column(Modifier.fillMaxWidth().clickable {model.jump(hit);onClose()}.padding(horizontal=20.dp,vertical=16.dp)){
                                        Text(hit.title,style=MaterialTheme.typography.labelMedium,color=MaterialTheme.colorScheme.primary)
                                        Spacer(Modifier.height(6.dp));Text(hit.excerpt,style=MaterialTheme.typography.bodyMedium)
                                    }
                                    HorizontalDivider(Modifier.padding(horizontal=20.dp),color=MaterialTheme.colorScheme.outlineVariant.copy(alpha=.4f),thickness=.5.dp)
                                }
                            }
                        }else Box(Modifier.fillMaxWidth().weight(1f),contentAlignment=Alignment.Center){Text(tr("搜索本书正文","Search the book text"),color=MaterialTheme.colorScheme.onSurfaceVariant)}
                    }else{
                        Text(state.active?.title.orEmpty(),Modifier.padding(horizontal=24.dp,vertical=20.dp),style=MaterialTheme.typography.titleMedium,fontWeight=FontWeight.SemiBold,maxLines=2,overflow=TextOverflow.Ellipsis)
                        HorizontalDivider(Modifier.padding(horizontal=20.dp),color=MaterialTheme.colorScheme.outlineVariant.copy(alpha=.5f),thickness=.5.dp)
                        if(visible.isEmpty())Box(Modifier.weight(1f).fillMaxWidth(),contentAlignment=Alignment.Center){Text(tr("本书暂无目录","No contents available"))}
                        else LazyColumn(state=contentsScroll,modifier=Modifier.weight(1f),contentPadding=PaddingValues(vertical=8.dp)){
                            items(visible,key={it.id}){entry->
                                val active=entry.id==current
                                Row(Modifier.fillMaxWidth().background(if(active)MaterialTheme.colorScheme.primaryContainer.copy(alpha=.65f)else Color.Transparent).heightIn(min=64.dp).padding(start=(12+entry.depth.toInt().coerceAtMost(5)*16).dp,end=20.dp),verticalAlignment=Alignment.CenterVertically){
                                    if(entry.hasChildren)ActionIcon(if(entry.id in collapsed)"next"else"expand",if(entry.id in collapsed)tr("展开章节","Expand chapter")else tr("收起章节","Collapse chapter"),selected=entry.id !in collapsed){collapsed=if(entry.id in collapsed)collapsed-entry.id else collapsed+entry.id}
                                    else Spacer(Modifier.width(36.dp))
                                    Column(Modifier.weight(1f).clickable(enabled=entry.href.isNotBlank()){keyboard?.hide();model.navigate(entry.href);onClose()}.padding(vertical=16.dp,horizontal=4.dp)){
                                        Text(entry.title,style=MaterialTheme.typography.bodyLarge,fontWeight=if(active)FontWeight.SemiBold else FontWeight.Normal,color=if(active)MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurface,maxLines=3,overflow=TextOverflow.Ellipsis)
                                        if(active){Spacer(Modifier.height(4.dp));Text(tr("当前读到","Reading here"),style=MaterialTheme.typography.labelSmall,color=MaterialTheme.colorScheme.primary)}
                                    }
                                }
                                HorizontalDivider(Modifier.padding(horizontal=20.dp),color=MaterialTheme.colorScheme.outlineVariant.copy(alpha=.35f),thickness=.5.dp)
                            }
                        }
                    }
                }
            }
        }
    }
}

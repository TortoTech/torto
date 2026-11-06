package org.tortotech.torto

import androidx.compose.foundation.*
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.semantics.*
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import org.tortotech.torto.core.TocItem

internal fun visibleContents(items:List<TocItem>,collapsed:Set<String>)=items.filter {entry->entry.ancestors.none {it in collapsed}}

/** Shared inline search field for book contents and local notes. */
@Composable internal fun RowScope.ReaderSearchInput(query:String,change:(String)->Unit,placeholder:String,focus:FocusRequester,enabled:Boolean=true,submit:()->Unit,cancel:()->Unit){
    ActionIcon("search",tr("搜索","Search"),query.isNotBlank()&&enabled,onClick=submit)
    BasicTextField(query,change,Modifier.weight(1f).fillMaxHeight().focusRequester(focus),singleLine=true,
        textStyle=MaterialTheme.typography.bodyLarge.copy(color=MaterialTheme.colorScheme.onSurface),cursorBrush=SolidColor(MaterialTheme.colorScheme.primary),
        keyboardOptions=KeyboardOptions(imeAction=ImeAction.Search),keyboardActions=KeyboardActions(onSearch={submit()}),
        decorationBox={input->Box(Modifier.fillMaxSize(),contentAlignment=Alignment.CenterStart){
            if(query.isEmpty())Text(placeholder,style=MaterialTheme.typography.bodyLarge,color=MaterialTheme.colorScheme.onSurfaceVariant)
            input()
        }})
    VerticalDivider(Modifier.height(22.dp).padding(horizontal=8.dp),color=MaterialTheme.colorScheme.outlineVariant)
    TextButton(onClick=cancel){Text(tr("取消","Cancel"),color=MaterialTheme.colorScheme.onSurfaceVariant)}
}

/** Reader-local drawer keeps the footer available; its state survives tab/visibility changes. */
@Composable internal fun ReadingNavigation(state:ScreenState,open:Boolean,model:ReaderModel,onClose:()->Unit){
    val book=state.active?.id
    var search by rememberSaveable(book){mutableStateOf(false)}
    var query by rememberSaveable(book){mutableStateOf("")}
    var submitted by rememberSaveable(book){mutableStateOf(false)}
    var collapsed by rememberSaveable(book,stateSaver=androidx.compose.runtime.saveable.listSaver<Set<String>,String>(save={it.toList()},restore={it.toSet()})){mutableStateOf<Set<String>>(emptySet())}
    var initialized by rememberSaveable(book){mutableStateOf(false)}
    var locatedCurrent by rememberSaveable(book){mutableStateOf<String?>(null)}
    val contentsScroll=rememberLazyListState()
    val searchScroll=rememberLazyListState()
    val current=state.frame?.info?.activeTocId
    val ancestors=state.frame?.info?.activeTocPath.orEmpty()
    val visible=remember(state.toc,collapsed){visibleContents(state.toc,collapsed)}
    val keyboard=LocalSoftwareKeyboardController.current
    val focusManager=LocalFocusManager.current
    val focus=remember {FocusRequester()}
    val pillHeight=with(LocalDensity.current){maxOf(40.dp,24.sp.toDp()+8.dp)}
    LaunchedEffect(open,book,state.toc.isNotEmpty(),current){
        if(open&&(!initialized||current!=locatedCurrent)){
            collapsed=(if(initialized)collapsed else state.toc.filter {it.hasChildren}.map {it.id}.toSet())-ancestors.toSet()
            initialized=true;locatedCurrent=current
            val at=visibleContents(state.toc,collapsed).indexOfFirst {it.id==current}
            if(!search&&at>=0){withFrameNanos {};contentsScroll.scrollToItem(at)}
        }
    }
    DisposableEffect(Unit){onDispose {focusManager.clearFocus();keyboard?.hide()}}
    LaunchedEffect(open,search){
        if(open&&search){withFrameNanos {};focus.requestFocus()}
        else {focusManager.clearFocus();keyboard?.hide()}
    }
    Column(Modifier.fillMaxSize()) {
        fun submit(){if(query.isNotBlank()&&!state.busy){submitted=true;focusManager.clearFocus();keyboard?.hide();model.search(query.trim())}}
        val headerLabel=tr("目录与搜索栏","Navigation header")
        Box(Modifier.padding(horizontal=16.dp).fillMaxWidth().height(pillHeight+8.dp).semantics {contentDescription=headerLabel},contentAlignment=Alignment.Center){
            Row(Modifier.fillMaxWidth().height(pillHeight).background(MaterialTheme.colorScheme.surfaceContainerHigh,RoundedCornerShape(20.dp)).padding(3.dp),verticalAlignment=Alignment.CenterVertically){
                if(search){
                    ReaderSearchInput(query,{query=it;submitted=false},tr("搜索本书内容","Search this book"),focus,!state.busy,::submit,{search=false})
                }else{
                    listOf(tr("搜本书","Search book"),tr("目录","Contents")).forEachIndexed {index,label->
                        val selected=index==1
                        Row(Modifier.weight(1f).fillMaxHeight().clip(RoundedCornerShape(20.dp)).background(if(selected)MaterialTheme.colorScheme.surface else Color.Transparent,RoundedCornerShape(20.dp)).semantics {this.selected=selected}.clickable(role=Role.Tab){search=index==0},horizontalArrangement=Arrangement.Center,verticalAlignment=Alignment.CenterVertically){
                            if(index==0){AppIcon("search",size=19.dp,tint=MaterialTheme.colorScheme.onSurfaceVariant);Spacer(Modifier.width(6.dp))}
                            Text(label,style=MaterialTheme.typography.labelLarge,color=if(selected)MaterialTheme.colorScheme.onSurface else MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                    }
                }
            }
        }
        if(search){
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
            }else Spacer(Modifier.weight(1f))
        }else{
            Text(state.active?.title.orEmpty(),Modifier.padding(start=48.dp,end=24.dp,top=20.dp,bottom=16.dp),style=MaterialTheme.typography.titleMedium.copy(fontSize=18.sp),fontWeight=FontWeight.SemiBold,maxLines=1,overflow=TextOverflow.Ellipsis)
            HorizontalDivider(Modifier.padding(horizontal=20.dp),color=MaterialTheme.colorScheme.outlineVariant.copy(alpha=.5f),thickness=.5.dp)
            if(visible.isEmpty())Box(Modifier.weight(1f).fillMaxWidth(),contentAlignment=Alignment.Center){Text(tr("本书暂无目录","No contents available"))}
            else LazyColumn(state=contentsScroll,modifier=Modifier.weight(1f),contentPadding=PaddingValues(vertical=8.dp)){
                items(visible,key={it.id}){entry->
                    val active=entry.id==current
                    val indent=entry.depth.toInt().coerceAtMost(5)*16
                    Row(Modifier.fillMaxWidth().heightIn(min=52.dp).padding(start=indent.dp,end=24.dp),verticalAlignment=Alignment.CenterVertically){
                        if(entry.hasChildren)ActionIcon(if(entry.id in collapsed)"next"else"expand",if(entry.id in collapsed)tr("展开章节","Expand chapter")else tr("收起章节","Collapse chapter")){collapsed=if(entry.id in collapsed)collapsed-entry.id else collapsed+entry.id}
                        else Spacer(Modifier.width(48.dp))
                        Text(entry.title,Modifier.weight(1f).clickable(enabled=entry.href.isNotBlank()){keyboard?.hide();model.navigateToc(entry.id);onClose()}.padding(vertical=14.dp),
                        style=MaterialTheme.typography.bodyLarge.copy(fontSize=17.sp),fontWeight=if(active)FontWeight.SemiBold else FontWeight.Normal,color=MaterialTheme.colorScheme.onSurface,maxLines=2,overflow=TextOverflow.Ellipsis)
                    }
                    if(active)Row(Modifier.fillMaxWidth().padding(start=(indent+64).dp,end=24.dp,top=8.dp,bottom=20.dp),verticalAlignment=Alignment.CenterVertically){
                        AppIcon("book",size=16.dp,tint=MaterialTheme.colorScheme.primary)
                        Spacer(Modifier.width(8.dp));Text(tr("当前读到","Reading here"),style=MaterialTheme.typography.labelLarge,color=MaterialTheme.colorScheme.primary)
                    }
                    HorizontalDivider(Modifier.padding(horizontal=20.dp),color=MaterialTheme.colorScheme.outlineVariant.copy(alpha=.35f),thickness=.5.dp)
                }
            }
        }
    }
}

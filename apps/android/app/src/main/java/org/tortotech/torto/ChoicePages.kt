package org.tortotech.torto

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.focusProperties
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.semantics.*
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

internal class ChoiceRequest(
    val owner:Any,
    val title:String,
    val value:()->String,
    val options:()->List<Pair<String,String>>,
    val enabled:()->Boolean,
    val choose:(String)->Unit,
    val searchable:Boolean=false,
)

@Stable internal class ChoicePages {
    var current by mutableStateOf<ChoiceRequest?>(null)
        private set
    val isOpen get()=current!=null
    fun open(request:ChoiceRequest){current=request}
    fun close(){current=null}
    fun release(owner:Any){if(current?.owner===owner)close()}
}
internal val LocalChoicePages=staticCompositionLocalOf<ChoicePages>{error("A choice page host is required")}

/** Full window navigation keeps the parent composed, preserving its drafts and
 * scroll position. Hidden parents cannot receive input or accessibility focus. */
@Composable internal fun ChoicePageHost(pages:ChoicePages=remember{ChoicePages()},content:@Composable ()->Unit){
    val focus=LocalFocusManager.current
    val keyboard=LocalSoftwareKeyboardController.current
    val request=pages.current
    LaunchedEffect(request){focus.clearFocus(force=true);keyboard?.hide()}
    CompositionLocalProvider(LocalChoicePages provides pages){
        Box(Modifier.fillMaxSize()){
            Box(Modifier.fillMaxSize().graphicsLayer {alpha=if(request==null)1f else 0f}
                .then(if(request!=null)Modifier.clearAndSetSemantics {}.focusProperties {canFocus=false}else Modifier)){
                content()
            }
            request?.let {key(it){ChoicePage(it,pages::close)}}
        }
        // Registered after the parent so system Back returns only one level.
        BackHandler(request!=null){pages.close()}
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable private fun ChoicePage(request:ChoiceRequest,back:()->Unit){
    var query by remember{mutableStateOf("")}
    val options=request.options()
    val visible=remember(options,query){if(query.isBlank())options else options.filter {it.first.contains(query,true)||it.second.contains(query,true)}}
    val current=request.value()
    val initialIndex=remember{(options.indexOfFirst {it.first==current}-2).coerceAtLeast(0)}
    val list=rememberLazyListState(initialIndex)
    val canSearch=request.searchable||options.size>8
    LaunchedEffect(query){list.scrollToItem(if(query.isBlank())initialIndex else 0)}
    Scaffold(Modifier.fillMaxSize().semantics {paneTitle=request.title}
        .pointerInput(Unit){awaitPointerEventScope {while(true){awaitPointerEvent(PointerEventPass.Final).changes.forEach {it.consume()}}}},
        containerColor=MaterialTheme.colorScheme.background,
        topBar={CenterAlignedTopAppBar(title={Text(request.title,fontSize=16.sp,maxLines=1,overflow=TextOverflow.Ellipsis)},
            navigationIcon={ActionIcon("back",tr("返回","Back"),onClick=back)},
            colors=TopAppBarDefaults.topAppBarColors(containerColor=MaterialTheme.colorScheme.background))},
    ){padding->
        Column(Modifier.fillMaxSize().padding(padding).imePadding().padding(horizontal=16.dp),verticalArrangement=Arrangement.spacedBy(12.dp)){
            if(canSearch)TextField(query,{query=it},Modifier.fillMaxWidth(),placeholder={Text(tr("搜索","Search"))},singleLine=true,
                leadingIcon={AppIcon("search")},trailingIcon={if(query.isNotEmpty())ActionIcon("close",tr("清空搜索","Clear search")){query=""}},
                shape=RoundedCornerShape(16.dp),colors=TextFieldDefaults.colors(
                    focusedContainerColor=MaterialTheme.colorScheme.surfaceContainerLowest,unfocusedContainerColor=MaterialTheme.colorScheme.surfaceContainerLowest,
                    focusedIndicatorColor=Color.Transparent,unfocusedIndicatorColor=Color.Transparent))
            if(!canSearch)Spacer(Modifier.height(0.dp))
            LazyColumn(Modifier.weight(1f).fillMaxWidth().clip(RoundedCornerShape(16.dp)),state=list,contentPadding=PaddingValues(bottom=16.dp)){
                itemsIndexed(visible,key={_,item->item.first}){index,(id,label)->
                    val selected=id==current
                    val corner=16.dp
                    val shape=RoundedCornerShape(topStart=if(index==0)corner else 0.dp,topEnd=if(index==0)corner else 0.dp,
                        bottomStart=if(index==visible.lastIndex)corner else 0.dp,bottomEnd=if(index==visible.lastIndex)corner else 0.dp)
                    Row(Modifier.fillMaxWidth().background(MaterialTheme.colorScheme.surfaceContainerLowest,shape)
                        .heightIn(min=56.dp).selectable(selected,enabled=request.enabled(),role=Role.RadioButton){if(id!=request.value())request.choose(id);back()}
                        .padding(horizontal=16.dp,vertical=16.dp),verticalAlignment=Alignment.CenterVertically){
                        Text(label,Modifier.weight(1f),fontSize=16.sp,fontWeight=if(selected)FontWeight.Medium else FontWeight.Normal,
                            color=if(selected)MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurface)
                        if(selected){Spacer(Modifier.width(12.dp));AppIcon("check",size=IconSizes.Small,tint=MaterialTheme.colorScheme.primary)}
                    }
                }
                if(visible.isEmpty())item {Text(if(query.isBlank())tr("暂无可选项","No options available")else tr("没有匹配结果","No matching results"),
                    Modifier.padding(16.dp),fontSize=14.sp,color=MaterialTheme.colorScheme.onSurfaceVariant)}
            }
        }
    }
}

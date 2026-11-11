package org.tortotech.torto

import androidx.compose.animation.*
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.collectIsPressedAsState
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.draw.clip
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.window.Popup
import androidx.compose.ui.window.PopupProperties
import androidx.compose.foundation.shape.RoundedCornerShape
import kotlinx.coroutines.delay

internal val ReaderBarHeight=56.dp

/** Opening and uncached turns belong to the content plane; quick loads stay quiet. */
@Composable internal fun ReaderOpeningIndicator(state:ScreenState){
    val visible=key(state.active?.id){loadingVisible(state.active!=null&&state.error==null&&(state.openingBook&&state.frame==null||state.readerContentLoading))}
    if(visible){
        val label=if(state.openingBook)tr("正在打开书籍","Opening book")else tr("正在加载内容","Loading content")
        Box(Modifier.fillMaxSize(),contentAlignment=Alignment.Center){
            CircularProgressIndicator(Modifier.size(28.dp).semantics {contentDescription=label},strokeWidth=2.dp)
        }
    }
}

/** Same safe area whether Android's system bars are visible or hidden. */
@OptIn(ExperimentalLayoutApi::class)
@Composable internal fun readerSafePadding():PaddingValues = WindowInsets.statusBarsIgnoringVisibility
    .union(WindowInsets.navigationBarsIgnoringVisibility).union(WindowInsets.displayCutout).asPaddingValues()

/** Focus reading occupies the entire screen height, including hidden system-bar areas.
 * Toolbar safe areas belong to the overlays; showing controls never resizes the page. */
@OptIn(ExperimentalLayoutApi::class)
@Composable internal fun readerContentPadding(focus:Boolean):PaddingValues =
    (if(focus)WindowInsets.displayCutout.only(WindowInsetsSides.Horizontal)
    else WindowInsets.displayCutout.union(WindowInsets.navigationBarsIgnoringVisibility.only(WindowInsetsSides.Bottom)))
        .asPaddingValues()

@Composable internal fun CompactHomeBar(selected:Int,onSelect:(Int)->Unit){
    Column(Modifier.fillMaxWidth().background(MaterialTheme.colorScheme.surface).navigationBarsPadding()){
        HorizontalDivider(color=MaterialTheme.colorScheme.outlineVariant.copy(alpha=.45f),thickness=.5.dp)
        Row(Modifier.fillMaxWidth().heightIn(min=ReaderBarHeight),verticalAlignment=Alignment.CenterVertically){
            listOf("library" to tr("书架","Library"),"person" to tr("我","Me")).forEachIndexed {index,(icon,label)->
                val active=selected==index
                val interaction=remember {MutableInteractionSource()}
                Column(Modifier.weight(1f).heightIn(min=56.dp).semantics {this.selected=active}.clickable(role=Role.Tab,interactionSource=interaction,indication=null,onClick={onSelect(index)}).padding(vertical=6.dp),horizontalAlignment=Alignment.CenterHorizontally,verticalArrangement=Arrangement.Center){
                    val pressed by interaction.collectIsPressedAsState()
                    val ink by animateColorAsState(if(active||pressed)brandControlColor() else defaultIconColor(),tween(100),label="home icon ink")
                    Box(Modifier.size(44.dp,IconSizes.Standard),contentAlignment=Alignment.Center){AppIcon(icon,tint=ink)}
                    Spacer(Modifier.height(2.dp))
                    Text(label,fontSize=11.sp,lineHeight=14.sp,color=if(active)MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
        }
    }
}

/** Overlay controls never participate in the native reading viewport's measurement. */
@Composable internal fun BoxScope.ReadingChrome(
    state:ScreenState,blocked:Boolean,panel:String?,onBack:()->Unit,onPanel:(String)->Unit,
    onTranslate:()->Unit,onTheme:()->Unit,onSplit:()->Unit={},onFeedback:()->Unit={},
){
    val safe=readerSafePadding()
    val bottomPanel=readingBottomPanel(panel)
    // Both bars paint the same surface through their system-bar safe areas.
    val toolbarBackground=MaterialTheme.colorScheme.surface
    AnimatedVisibility(visible=state.chromeVisible&&!bottomPanel,enter=fadeIn(tween(180)),exit=fadeOut(tween(180)),modifier=Modifier.align(Alignment.TopCenter)){
        Column(Modifier.fillMaxWidth().background(toolbarBackground).padding(top=safe.calculateTopPadding()).height(ReaderBarHeight)){
            Row(Modifier.fillMaxWidth().weight(1f).padding(horizontal=8.dp),verticalAlignment=Alignment.CenterVertically){
                ActionIcon("back",tr("返回","Back"),onClick=onBack)
                Spacer(Modifier.weight(1f))
                ActionIcon("feedback",tr("反馈问题","Report an issue"),onClick=onFeedback)
            }
            HorizontalDivider(color=MaterialTheme.colorScheme.outlineVariant.copy(alpha=.45f),thickness=.5.dp)
        }
    }
    AnimatedVisibility(visible=state.chromeVisible||bottomPanel,enter=fadeIn(tween(180))+slideInVertically(tween(180)){it/5},exit=fadeOut(tween(180))+slideOutVertically(tween(180)){it/5},modifier=Modifier.align(Alignment.BottomCenter)){
        Column(Modifier.fillMaxWidth().background(toolbarBackground).padding(bottom=safe.calculateBottomPadding())){
            HorizontalDivider(color=MaterialTheme.colorScheme.outlineVariant.copy(alpha=.45f),thickness=.5.dp)
            Row(Modifier.fillMaxWidth().height(ReaderBarHeight),horizontalArrangement=Arrangement.SpaceEvenly,verticalAlignment=Alignment.CenterVertically){
                ActionIcon("toc",tr("目录与搜索","Contents and search"),selected=panel=="navigation"){onPanel("navigation")}
                ActionIcon("marks",tr("批注与高亮","Notes and highlights"),!state.chatWorking,selected=readingPanelOwner(panel)=="marks"){onPanel("marks")}
                var selectionHint by remember {mutableStateOf(false)}
                LaunchedEffect(selectionHint){if(selectionHint){delay(2000);selectionHint=false}}
                Box {
                    ActionIcon("select",tr("文字选择模式","Selection mode")){
                        if(state.translated)selectionHint=true else onPanel("selection")
                    }
                    if(selectionHint){
                        val offset=with(LocalDensity.current){(-52).dp.roundToPx()}
                        Popup(alignment=Alignment.TopCenter,offset=IntOffset(0,offset),onDismissRequest={selectionHint=false},properties=PopupProperties(focusable=false)){
                            Surface(shape=RoundedCornerShape(6.dp),color=MaterialTheme.colorScheme.inverseSurface,contentColor=MaterialTheme.colorScheme.inverseOnSurface,shadowElevation=2.dp){
                                Text(tr("翻译时仅支持按段选择","Only paragraph selection is available during translation"),Modifier.padding(horizontal=12.dp,vertical=8.dp).widthIn(max=260.dp),fontSize=13.sp)
                            }
                        }
                    }
                }
                ActionIcon("type",tr("字体排版","Typography"),selected=readingPanelOwner(panel)=="typography"){onPanel("typography")}
                ActionIcon(if(state.dark)"sun" else "moon",tr("切换明暗模式","Toggle light/dark"),!blocked,onClick=onTheme)
            }
        }
    }
    AnimatedVisibility(visible=state.chromeVisible&&panel==null&&state.interaction==null&&state.selected==null,enter=fadeIn(tween(180))+slideInVertically(tween(180)){it/5},exit=fadeOut(tween(180))+slideOutVertically(tween(180)){it/5},modifier=Modifier.align(Alignment.BottomEnd).padding(end=16.dp,bottom=safe.calculateBottomPadding()+ReaderBarHeight+16.dp)){
        val split=state.frame?.info?.controls?.find {it.kind=="sentences"}
        val showTranslation=showReadingTranslation(state.bookLanguages,LocalConfiguration.current.locales[0],state.translated)
        Row(horizontalArrangement=Arrangement.spacedBy(10.dp)){
            ReadingFloatingAction("ai",tr("阅读助手","Reading assistant"),!blocked){onPanel("chat")}
            ReadingFloatingAction("sentences",if(split?.active==true)tr("取消当前段落分句","Restore current paragraph")else tr("当前段落按句分段","Split current paragraph"),!blocked&&split!=null,split?.active==true,onSplit)
            if(showTranslation)ReadingFloatingAction("translate",if(state.translated)tr("关闭翻译","Turn translation off")else tr("开启翻译","Turn translation on"),!blocked,state.translated,onTranslate)
        }
    }
}

/** Slate circles remain visible over both light and dark reading content. */
@Composable internal fun ReadingFloatingAction(icon:String,label:String,enabled:Boolean,active:Boolean=false,onClick:()->Unit){
    val interaction=remember {MutableInteractionSource()}
    val pressed by interaction.collectIsPressedAsState()
    val ink by animateColorAsState((if(active||pressed)brandControlColor() else Color.White).copy(alpha=if(!enabled).38f else 1f),tween(100),label="floating action ink")
    Box(Modifier.size(IconSizes.TouchTarget).clip(CircleShape).background(Color(0xff5e646e)).semantics {selected=active}
        .clickable(enabled=enabled,role=Role.Button,interactionSource=interaction,indication=null,onClick=onClick),contentAlignment=Alignment.Center){
        AppIcon(icon,description=label,tint=ink)
    }
}

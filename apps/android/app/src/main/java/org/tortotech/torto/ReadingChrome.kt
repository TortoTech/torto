package org.tortotech.torto

import androidx.compose.animation.*
import androidx.compose.animation.core.tween
import androidx.compose.animation.core.updateTransition
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

internal val ReaderBarHeight=56.dp

/** Same safe area whether Android's system bars are visible or hidden. */
@OptIn(ExperimentalLayoutApi::class)
@Composable internal fun readerSafePadding():PaddingValues = WindowInsets.statusBarsIgnoringVisibility
    .union(WindowInsets.navigationBarsIgnoringVisibility).union(WindowInsets.displayCutout).asPaddingValues()

@Composable internal fun CompactHomeBar(selected:Int,onSelect:(Int)->Unit){
    Column(Modifier.fillMaxWidth().background(MaterialTheme.colorScheme.surface).navigationBarsPadding()){
        HorizontalDivider(color=MaterialTheme.colorScheme.outlineVariant.copy(alpha=.45f),thickness=.5.dp)
        Row(Modifier.fillMaxWidth().heightIn(min=ReaderBarHeight),verticalAlignment=Alignment.CenterVertically){
            listOf("library" to tr("书架","Library"),"person" to tr("我","Me")).forEachIndexed {index,(icon,label)->
                val active=selected==index
                Column(Modifier.weight(1f).heightIn(min=56.dp).semantics {this.selected=active}.clickable(role=Role.Tab,onClick={onSelect(index)}).padding(vertical=6.dp),horizontalAlignment=Alignment.CenterHorizontally,verticalArrangement=Arrangement.Center){
                    Box(Modifier.size(44.dp,26.dp).background(if(active)MaterialTheme.colorScheme.primaryContainer else Color.Transparent,RoundedCornerShape(14.dp)),contentAlignment=Alignment.Center){
                        AppIcon(icon,modifier=Modifier.size(22.dp),tint=if(active)MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,filled=active)
                    }
                    Spacer(Modifier.height(2.dp))
                    Text(label,fontSize=11.sp,lineHeight=14.sp,color=if(active)MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
        }
    }
}

/** Overlay controls never participate in the native reading viewport's measurement. */
@Composable internal fun BoxScope.ReadingChrome(
    state:ScreenState,blocked:Boolean,processing:Boolean,panel:String?,onBack:()->Unit,onPanel:(String)->Unit,
    onTranslate:()->Unit,onTheme:()->Unit,onStop:()->Unit,
){
    val safe=readerSafePadding()
    val transition=updateTransition(state.chromeVisible,label="reading controls")
    transition.AnimatedVisibility(visible={it},enter=fadeIn(tween(180)),exit=fadeOut(tween(180)),modifier=Modifier.align(Alignment.TopCenter)){
        Row(Modifier.fillMaxWidth().background(MaterialTheme.colorScheme.background).padding(top=safe.calculateTopPadding()).height(ReaderBarHeight).padding(horizontal=8.dp),verticalAlignment=Alignment.CenterVertically){
            ActionIcon("back",tr("返回","Back"),onClick=onBack)
            Spacer(Modifier.weight(1f))
            if(processing)ActionIcon("stop",tr("停止处理","Stop processing"),onClick=onStop)
            ActionIcon("chat",tr("阅读助手","Reading assistant"),!blocked,selected=panel=="chat",onClick={onPanel("chat")})
        }
    }
    transition.AnimatedVisibility(visible={it},enter=fadeIn(tween(180))+slideInVertically(tween(180)){it/5},exit=fadeOut(tween(180))+slideOutVertically(tween(180)){it/5},modifier=Modifier.align(Alignment.BottomCenter)){
        Column(Modifier.fillMaxWidth().background(MaterialTheme.colorScheme.surface).padding(bottom=safe.calculateBottomPadding())){
            HorizontalDivider(color=MaterialTheme.colorScheme.outlineVariant.copy(alpha=.45f),thickness=.5.dp)
            Row(Modifier.fillMaxWidth().height(ReaderBarHeight),horizontalArrangement=Arrangement.SpaceEvenly,verticalAlignment=Alignment.CenterVertically){
                ActionIcon("toc",tr("目录与搜索","Contents and search"),selected=panel=="navigation"){onPanel("navigation")}
                ActionIcon("marks",tr("批注与高亮","Notes and highlights"),!state.chatWorking,selected=panel=="marks"){onPanel("marks")}
                ActionIcon("select",tr("文字选择模式","Selection mode"),!state.translated,selected=panel=="selection"){onPanel("selection")}
                ActionIcon("type",tr("字体排版","Typography"),selected=panel=="typography"){onPanel("typography")}
                ActionIcon(if(state.dark)"sun" else "moon",tr("切换明暗模式","Toggle light/dark"),!blocked,onClick=onTheme)
            }
        }
    }
    transition.AnimatedVisibility(visible={it&&panel==null&&state.interaction==null&&state.selected==null},enter=fadeIn(tween(180))+slideInVertically(tween(180)){it/5},exit=fadeOut(tween(180))+slideOutVertically(tween(180)){it/5},modifier=Modifier.align(Alignment.BottomEnd).padding(end=16.dp,bottom=safe.calculateBottomPadding()+ReaderBarHeight+16.dp)){
        Surface(shape=RoundedCornerShape(18.dp),color=if(state.translated)MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.surfaceContainerHigh,
            shadowElevation=3.dp,border=BorderStroke(.5.dp,MaterialTheme.colorScheme.outlineVariant.copy(alpha=.5f))){
            val ink=if(state.translated)MaterialTheme.colorScheme.onPrimary else MaterialTheme.colorScheme.primary
            IconButton(onClick=onTranslate,enabled=!blocked,modifier=Modifier.size(52.dp),colors=IconButtonDefaults.iconButtonColors(contentColor=ink,disabledContentColor=ink.copy(alpha=.38f))){
                AppIcon("translate",if(state.translated)tr("关闭翻译","Turn translation off")else tr("开启翻译","Turn translation on"))
            }
        }
    }
}

package org.tortotech.torto

import androidx.compose.foundation.clickable
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** Pinned note navigation overlays the existing page without changing pagination. */
@Composable internal fun BoxScope.NoteReadingChrome(state:ScreenState,onList:()->Unit,onClose:()->Unit,onPrevious:()->Unit,onNext:()->Unit){
    val safe=readerSafePadding()
    val index=state.annotations.indexOfFirst {it.id==state.annotationPreviewId}
    val note=state.annotations.getOrNull(index)
    val border=BorderStroke(.5.dp,MaterialTheme.colorScheme.outlineVariant.copy(alpha=.35f))
    val bottomHeight=maxOf(40.dp,with(LocalDensity.current){12.sp.toDp()}+24.dp)
    // The visible surface is compact; transparent 48dp targets extend beyond it.
    Box(Modifier.align(Alignment.TopCenter).padding(start=16.dp,end=16.dp,top=safe.calculateTopPadding()+12.dp).fillMaxWidth().heightIn(min=48.dp)){
        Surface(Modifier.align(Alignment.Center).fillMaxWidth(),shape=RoundedCornerShape(10.dp),color=MaterialTheme.colorScheme.surface.copy(alpha=.97f),border=border){
            Column(Modifier.fillMaxWidth().heightIn(min=40.dp).padding(horizontal=52.dp,vertical=4.dp),horizontalAlignment=Alignment.CenterHorizontally,verticalArrangement=Arrangement.Center){
                Text(tr("笔记","Notes"),fontSize=16.sp,lineHeight=18.sp)
                note?.chapterTitle?.takeIf {it.isNotBlank()}?.let {title->
                    Text(title,fontSize=11.sp,lineHeight=13.sp,color=MaterialTheme.colorScheme.onSurfaceVariant,maxLines=1,overflow=TextOverflow.Ellipsis,textAlign=TextAlign.Center)
                }
            }
        }
        ActionIcon("toc",tr("返回笔记列表","Back to notes"),!state.busy,modifier=Modifier.align(Alignment.CenterStart).padding(start=4.dp),iconSize=16.667.dp,onClick=onList)
        ActionIcon("close",tr("退出笔记查看","Exit note view"),modifier=Modifier.align(Alignment.CenterEnd).padding(end=4.dp),iconSize=15.dp,onClick=onClose)
    }
    Box(Modifier.align(Alignment.BottomCenter).padding(start=16.dp,end=16.dp,bottom=safe.calculateBottomPadding()+12.dp).fillMaxWidth().heightIn(min=maxOf(48.dp,bottomHeight))){
        Surface(Modifier.align(Alignment.Center).fillMaxWidth().height(bottomHeight),shape=RoundedCornerShape(10.dp),color=MaterialTheme.colorScheme.surface.copy(alpha=.97f),border=border){}
        Row(Modifier.fillMaxWidth().heightIn(min=maxOf(48.dp,bottomHeight)),verticalAlignment=Alignment.CenterVertically){
            NoteNavigationAction("marks",tr("笔记列表","Notes list"),!state.busy,onList)
            VerticalDivider(Modifier.height(20.dp),thickness=.5.dp,color=MaterialTheme.colorScheme.outlineVariant.copy(alpha=.35f))
            NoteNavigationAction("back",tr("上一个笔记","Previous note"),!state.busy&&index>0,onPrevious)
            VerticalDivider(Modifier.height(20.dp),thickness=.5.dp,color=MaterialTheme.colorScheme.outlineVariant.copy(alpha=.35f))
            NoteNavigationAction("next",tr("下一个笔记","Next note"),!state.busy&&index>=0&&index<state.annotations.lastIndex,onNext)
        }
    }
}

@Composable private fun RowScope.NoteNavigationAction(icon:String,label:String,enabled:Boolean,action:()->Unit){
    val color=MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha=if(enabled)1f else .38f)
    Column(Modifier.weight(1f).heightIn(min=48.dp).clickable(enabled=enabled,role=Role.Button,onClick=action).padding(horizontal=8.dp,vertical=2.dp),horizontalAlignment=Alignment.CenterHorizontally,verticalArrangement=Arrangement.Center){
        AppIcon(if(icon=="next")"back"else icon,description=label,modifier=if(icon=="next")Modifier.graphicsLayer(rotationZ=180f)else Modifier,size=15.dp,tint=color)
        Text(label,fontSize=10.sp,lineHeight=12.sp,color=color,textAlign=TextAlign.Center,maxLines=1,overflow=TextOverflow.Ellipsis)
    }
}

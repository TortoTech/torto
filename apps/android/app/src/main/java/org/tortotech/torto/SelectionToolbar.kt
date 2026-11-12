package org.tortotech.torto

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.*
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Popup
import androidx.compose.ui.window.PopupProperties
import org.tortotech.torto.core.SelectedText

/** Compact dark selection surface, in the same plane as native selection bounds. */
@Composable internal fun BoxScope.SelectionToolbar(
    selection: SelectedText,
    onCopy: () -> Unit,
    onDismiss: () -> Unit,
    onHighlight: (() -> Unit)? = null,
    onNote: (() -> Unit)? = null,
    outsideClip: Boolean = false,
) {
    BoxWithConstraints(Modifier.matchParentSize()) {
        val visible=selection.bounds.filter {
            it.y+it.height>0&&it.y<maxHeight.value&&it.x+it.width>0&&it.x<maxWidth.value
        }
        val anchor=visible.firstOrNull() ?: return@BoxWithConstraints
        val count=1+listOf(onHighlight,onNote).count {it!=null}
        val width=(count*68+12).dp
        val height=64.dp
        val left=(anchor.x+anchor.width/2-width.value/2).dp.coerceIn(8.dp,(maxWidth-width-8.dp).coerceAtLeast(8.dp))
        val above=anchor.y.dp-height-12.dp
        val below=(visible.last().y+visible.last().height).dp+12.dp
        val top=if(outsideClip)above else when {
            above>=8.dp->above
            below+height<=maxHeight-8.dp->below
            else->(maxHeight-height-16.dp).coerceAtLeast(8.dp)
        }
        val title=tr("划选工具栏","Selection actions")
        val surface:@Composable (Modifier)->Unit={modifier->
            Surface(modifier.width(width).semantics {paneTitle=title},shape=RoundedCornerShape(18.dp),color=Color(0xFF3D3D3D),contentColor=Color.White,shadowElevation=6.dp){
                Row(Modifier.padding(horizontal=6.dp)){
                    SelectionAction("copy",tr("复制","Copy"),onCopy)
                    onHighlight?.let {SelectionAction(if(selection.highlighted)"remove-underline"else"underline",if(selection.highlighted)tr("删除划线","Remove highlight")else tr("划线","Highlight"),it,label=if(selection.highlighted)tr("删除划线","Remove")else tr("划线","Highlight"))}
                    onNote?.let {SelectionAction("thought",tr("写想法","Note"),it)}
                }
            }
        }
        // A short reference needs only Copy. Keep its floating surface outside
        // the document's clip, without reserving space in the document itself.
        if(outsideClip){
            val density=LocalDensity.current
            Popup(alignment=Alignment.TopStart,offset=with(density){IntOffset(left.roundToPx(),top.roundToPx())},
                onDismissRequest=onDismiss,properties=PopupProperties(focusable=false,dismissOnClickOutside=true)){
                surface(Modifier)
            }
        }else surface(Modifier.offset(left,top))
    }
}

@Composable private fun SelectionAction(icon:String,title:String,action:()->Unit,label:String=title){
    Column(Modifier.width(68.dp).height(64.dp).clickable(role=Role.Button,onClick=action).semantics(mergeDescendants=true){contentDescription=title},
        horizontalAlignment=Alignment.CenterHorizontally,verticalArrangement=Arrangement.Center){
        AppIcon(icon,size=24.dp,tint=Color.White)
        Spacer(Modifier.height(5.dp))
        Text(label,Modifier.fillMaxWidth(),fontSize=12.sp,lineHeight=16.sp,color=Color.White,maxLines=1,textAlign=TextAlign.Center,overflow=TextOverflow.Ellipsis)
    }
}

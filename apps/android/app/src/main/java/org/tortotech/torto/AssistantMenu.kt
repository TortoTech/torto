package org.tortotech.torto

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.*
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.*
import androidx.compose.ui.window.Popup
import androidx.compose.ui.window.PopupPositionProvider
import androidx.compose.ui.window.PopupProperties

internal class AssistantMenuPosition(private val gap:Int,private val margin:Int,private val above:Boolean):PopupPositionProvider {
    override fun calculatePosition(anchorBounds:IntRect,windowSize:IntSize,layoutDirection:LayoutDirection,popupContentSize:IntSize):IntOffset {
        val x=if(layoutDirection==LayoutDirection.Ltr)anchorBounds.right-popupContentSize.width else anchorBounds.left
        val top=anchorBounds.top-gap-popupContentSize.height
        val bottom=anchorBounds.bottom+gap
        val y=if(above||bottom+popupContentSize.height>windowSize.height-margin)top else bottom
        return IntOffset(x.coerceIn(margin,maxOf(margin,windowSize.width-margin-popupContentSize.width)),y.coerceIn(margin,maxOf(margin,windowSize.height-margin-popupContentSize.height)))
    }
}
@Composable internal fun AssistantMenu(expanded:Boolean,dismiss:()->Unit,above:Boolean,content:@Composable ColumnScope.()->Unit){
    if(!expanded)return
    val density=LocalDensity.current
    // Padding leaves room for the popup's shadow; its visible bottom sits 8dp
    // above the answer button, rather than clipping the shadow at window edges.
    val position=remember(density,above){with(density){AssistantMenuPosition(0,12.dp.roundToPx(),above)}}
    val label=tr("更多操作","More actions")
    Popup(popupPositionProvider=position,onDismissRequest=dismiss,properties=PopupProperties(focusable=true)){
        Box(Modifier.padding(8.dp)){Surface(shape=RoundedCornerShape(16.dp),color=MaterialTheme.colorScheme.surfaceContainerLowest,shadowElevation=12.dp,
            border=androidx.compose.foundation.BorderStroke(.5.dp,MaterialTheme.colorScheme.outlineVariant.copy(alpha=.25f)),modifier=Modifier.semantics {paneTitle=label}){
            Column(Modifier.widthIn(min=176.dp,max=264.dp).width(IntrinsicSize.Max).padding(vertical=6.dp),content=content)
        }}
    }
}
@Composable internal fun AssistantMenuItem(icon:String,title:String,enabled:Boolean=true,action:()->Unit){
    val ink=MaterialTheme.colorScheme.onSurface.copy(alpha=if(enabled)1f else .38f)
    Row(Modifier.fillMaxWidth().heightIn(min=48.dp).clickable(enabled=enabled,role=Role.Button,onClick=action).padding(horizontal=16.dp,vertical=10.dp),verticalAlignment=Alignment.CenterVertically){
        AppIcon(icon,size=20.dp,tint=ink)
        Spacer(Modifier.width(10.dp))
        Text(title,fontSize=14.sp,lineHeight=20.sp,color=ink,maxLines=1,overflow=TextOverflow.Ellipsis)
    }
}

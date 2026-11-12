package org.tortotech.torto

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.semantics.*
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

@Composable internal fun readingSliderColor()=if(MaterialTheme.colorScheme.background.luminance()<.5f)
    MaterialTheme.colorScheme.surfaceContainerHigh else Color(0xffeceeee)

/** One visual and accessibility implementation for reader and settings controls. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable internal fun ReadingCapsuleSlider(
    value:Float,enabled:Boolean,label:String,range:ClosedFloatingPointRange<Float>,steps:Int=0,
    thumbText:String?=null,onChange:(Float)->Unit,onCommit:(Float)->Unit,
    leading:@Composable ()->Unit,trailing:@Composable ()->Unit,
){
    var pending by remember(value){mutableFloatStateOf(value)}
    Box(Modifier.fillMaxWidth().height(48.dp)){
        Box(Modifier.fillMaxWidth().height(36.dp).align(Alignment.Center).background(readingSliderColor(),CircleShape))
        Row(Modifier.fillMaxSize(),verticalAlignment=Alignment.CenterVertically){
            leading()
            Slider(value,{pending=it;onChange(it)},Modifier.weight(1f).fillMaxHeight().semantics {
                contentDescription=label
                thumbText?.let {stateDescription=it}
            },enabled=enabled,valueRange=range,steps=steps,onValueChangeFinished={onCommit(pending)},
                thumb={
                    Box(Modifier.size(32.dp).shadow(2.dp,CircleShape).background(Color.White,CircleShape).border(1.dp,Color.Black.copy(alpha=.05f),CircleShape),contentAlignment=Alignment.Center){
                        thumbText?.let {Text(it,fontSize=14.sp,lineHeight=18.sp,fontWeight=FontWeight.Medium,color=Color(0xff252a32).copy(alpha=if(enabled)1f else .38f))}
                    }
                },track={Spacer(Modifier.fillMaxWidth().height(36.dp))})
            trailing()
        }
    }
}

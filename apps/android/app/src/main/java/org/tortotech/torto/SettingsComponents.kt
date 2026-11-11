package org.tortotech.torto

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** Rows share the enclosing group's surface and edge alignment. */
@Composable internal fun SettingsGroup(content:@Composable ColumnScope.()->Unit){
    Surface(Modifier.fillMaxWidth(),shape=RoundedCornerShape(16.dp),color=MaterialTheme.colorScheme.surfaceContainerLowest){
        Column(content=content)
    }
}

@Composable internal fun SettingsItem(title:String,value:String?=null,description:String?=null,icon:String?=null,enabled:Boolean=true,onClick:()->Unit){
    val ink=MaterialTheme.colorScheme.onSurface.copy(alpha=if(enabled)1f else .38f)
    val secondary=MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha=if(enabled)1f else .38f)
    BoxWithConstraints(Modifier.fillMaxWidth()){
    val valueWidth=(maxWidth-32.dp)*.52f
    Row(Modifier.fillMaxWidth().heightIn(min=56.dp).clickable(enabled=enabled,onClick=onClick).padding(horizontal=16.dp,vertical=16.dp),verticalAlignment=Alignment.CenterVertically){
        val iconInk=defaultIconColor().copy(alpha=if(enabled)1f else .38f)
        if(icon!=null){AppIcon(icon,size=IconSizes.Compact,tint=iconInk);Spacer(Modifier.width(12.dp))}
        Column(Modifier.weight(1f)){
            Text(title,fontSize=16.sp,color=ink)
            if(description!=null){Spacer(Modifier.height(4.dp));Text(description,fontSize=14.sp,color=secondary)}
        }
        if(value!=null){
            Spacer(Modifier.width(12.dp))
            Text(value,fontSize=14.sp,color=secondary,maxLines=2,overflow=TextOverflow.Ellipsis,modifier=Modifier.widthIn(max=valueWidth),textAlign=androidx.compose.ui.text.style.TextAlign.End)
        }
        Spacer(Modifier.width(4.dp))
        // Disclosure arrows use their actual rendered size.
        AppIcon("next",size=IconSizes.Chevron,tint=iconInk)
    }
    }
}

@Composable internal fun SettingsSwitch(title:String,checked:Boolean,enabled:Boolean=true,description:String?=null,change:(Boolean)->Unit){
    Row(Modifier.fillMaxWidth().heightIn(min=56.dp).toggleable(value=checked,enabled=enabled,role=Role.Switch,onValueChange=change).padding(horizontal=16.dp,vertical=10.dp),verticalAlignment=Alignment.CenterVertically){
        Column(Modifier.weight(1f)){
            Text(title,fontSize=16.sp,color=MaterialTheme.colorScheme.onSurface.copy(alpha=if(enabled)1f else .38f))
            if(description!=null){Spacer(Modifier.height(4.dp));Text(description,fontSize=14.sp,color=MaterialTheme.colorScheme.onSurfaceVariant)}
        }
        Spacer(Modifier.width(16.dp))
        Switch(checked,onCheckedChange=null,enabled=enabled,colors=tortoSwitchColors())
    }
}

@Composable internal fun SettingsSlider(title:String,valueLabel:String,value:Float,onValueChange:(Float)->Unit,valueRange:ClosedFloatingPointRange<Float>,enabled:Boolean=true,steps:Int=0,onValueChangeFinished:(()->Unit)?=null){
    Column(Modifier.fillMaxWidth().padding(horizontal=16.dp,vertical=12.dp)){
        Row(verticalAlignment=Alignment.CenterVertically){
            Text(title,Modifier.weight(1f),fontSize=16.sp)
            Spacer(Modifier.width(12.dp))
            Text(valueLabel,fontSize=14.sp,color=MaterialTheme.colorScheme.onSurfaceVariant)
        }
        Slider(value,onValueChange,valueRange=valueRange,enabled=enabled,steps=steps,onValueChangeFinished=onValueChangeFinished)
    }
}

@Composable internal fun SettingsNote(text:String){
    Text(text,Modifier.padding(horizontal=16.dp),style=MaterialTheme.typography.bodySmall,color=MaterialTheme.colorScheme.onSurfaceVariant)
}

package org.tortotech.torto

import android.view.Window
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.*
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/** Window-only override: no system settings permission and no brightness leak onto other pages. */
@Composable internal fun ReadingWindowBrightness(window:Window?,reading:Boolean,value:Float){
    DisposableEffect(window,reading){
        val original=window?.attributes?.screenBrightness
        onDispose {if(reading&&original!=null)window?.let {it.attributes=it.attributes.apply {screenBrightness=original}}}
    }
    SideEffect {
        if(reading)window?.let {
            val brightness=if(value<0f)-1f else value.coerceIn(.01f,1f)
            if(it.attributes.screenBrightness!=brightness)it.attributes=it.attributes.apply {screenBrightness=brightness}
        }
    }
}

@Composable internal fun ReadingThemePanel(state:ScreenState,model:ReaderModel){
    val context=LocalContext.current
    val system by produceState(.5f,context){
        value=withContext(Dispatchers.IO){
            runCatching {android.provider.Settings.System.getInt(context.contentResolver,android.provider.Settings.System.SCREEN_BRIGHTNESS)/255f}.getOrDefault(.5f).coerceIn(.01f,1f)
        }
    }
    val brightness=state.readingBrightness.takeIf {it>=0f}?:system
    val label=tr("亮度","Brightness")
    Column(Modifier.fillMaxWidth().padding(horizontal=20.dp,vertical=20.dp),verticalArrangement=Arrangement.spacedBy(20.dp)){
        ReadingCapsuleSlider(brightness,true,label,.01f..1f,onChange={model.setReadingBrightness(it,persist=false)},onCommit={model.setReadingBrightness(it)},leading={
                Box(Modifier.width(40.dp),contentAlignment=Alignment.Center){AppIcon("sun",size=20.dp,tint=defaultIconColor())}
            },trailing={Spacer(Modifier.width(4.dp))})
        Column(verticalArrangement=Arrangement.spacedBy(8.dp)){
            Text(tr("颜色","Color"),fontSize=12.sp,color=MaterialTheme.colorScheme.onSurfaceVariant)
            Row(Modifier.fillMaxWidth(),horizontalArrangement=Arrangement.spacedBy(12.dp)){
                for(dark in listOf(false,true)){
                    val selected=state.dark==dark
                    val accent=brandControlColor()
                    val title=if(dark)tr("深色","Dark")else tr("浅色","Light")
                    Surface(Modifier.weight(1f),shape=RoundedCornerShape(12.dp),color=if(dark)Color(0xff222831)else Color.White,
                        border=BorderStroke(if(selected)2.dp else 1.dp,if(selected)accent else MaterialTheme.colorScheme.outlineVariant.copy(alpha=.4f))){
                        Row(Modifier.fillMaxWidth().heightIn(min=56.dp).selectable(selected,enabled=!state.busy,role=Role.RadioButton){if(!selected)model.configure(state.fontSize,state.unified,dark)}.padding(12.dp),
                            verticalAlignment=Alignment.CenterVertically,horizontalArrangement=Arrangement.Center){
                            AppIcon(if(dark)"moon"else "sun",size=20.dp,tint=if(selected)accent else if(dark)Color.White else defaultIconColor())
                            Spacer(Modifier.width(8.dp))
                            Text(title,fontSize=14.sp,color=if(selected)accent else if(dark)Color.White else Color(0xff252a32))
                        }
                    }
                }
            }
        }
    }
}

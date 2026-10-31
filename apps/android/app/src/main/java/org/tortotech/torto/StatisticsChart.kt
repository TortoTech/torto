package org.tortotech.torto

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import java.time.LocalDate
import kotlin.math.*
import org.tortotech.torto.core.DayReading

/** Presentation scale matches the original app: three readable ticks plus headroom. */
private fun distributionStep(maximum:Double):Double {
    val minutes=maximum/60000*1.08/3
    for(step in listOf(1.0,2.0,5.0,10.0,15.0,30.0,60.0,120.0,180.0,300.0,600.0,1200.0))if(step>=minutes)return step*60000
    val hours=minutes/60;val magnitude=10.0.pow(floor(log10(hours)))
    return listOf(1.0,2.0,5.0,10.0).first {it*magnitude>=hours}*magnitude*3600000
}
private fun distributionTick(value:Double,step:Double):String {
    if(value==0.0)return "0"
    val hours=step>=3600000
    val number=String.format(java.util.Locale.ROOT,"%.2f",value/(if(hours)3600000 else 60000)).trimEnd('0').trimEnd('.')
    return number+if(hours)"h" else "min"
}
@Composable internal fun ReadingDistribution(days:List<DayReading>,period:String,parentScrolling:Boolean=false) {
    var selected by remember(days,period){mutableStateOf<Int?>(null)}
    val scroll=rememberScrollState()
    LaunchedEffect(days,period){selected=null;scroll.scrollTo(0)}
    LaunchedEffect(parentScrolling,scroll.isScrollInProgress){if(parentScrolling||scroll.isScrollInProgress)selected=null}
    val colors=MaterialTheme.colorScheme
    val density=LocalDensity.current
    val maximum=days.maxOfOrNull {it.milliseconds.toDouble()} ?: 0.0
    val step=distributionStep(maximum);val ceiling=step*3
    val ticks=(0..3).map {distributionTick(it*step,step)}
    val weekdays=tr("一,二,三,四,五,六,日","Mon,Tue,Wed,Thu,Fri,Sat,Sun").split(',')
    val labels=days.map {day->when(period) {
        "week"->runCatching {weekdays[LocalDate.parse(day.day).dayOfWeek.value-1]}.getOrDefault("")
        "month"->day.day.takeLast(2).toIntOrNull()?.let {if(it==1||it%5==0)it.toString() else ""}.orEmpty()
        "year"->day.day.substring(5,7).toIntOrNull()?.toString().orEmpty()
        else->day.day.take(4)
    }}
    val style=MaterialTheme.typography.labelSmall.copy(fontSize=11.sp,color=colors.onSurfaceVariant)
    val measurer=rememberTextMeasurer()
    val axisWidth=with(density){ticks.maxOf {measurer.measure(it,style).size.width}.toDp()}
    val labelHeight=with(density){measurer.measure("00",style).size.height.toDp()}
    val plotHeight=190.dp
    Row(Modifier.fillMaxWidth().padding(top=24.dp),verticalAlignment=Alignment.Top) {
        BoxWithConstraints(Modifier.weight(1f)) {
            val viewportWidth=maxWidth
            val minimum=when(period){"month"->8.dp;"all"->66.dp;else->28.dp}
            val width=maxOf(maxWidth,minimum*days.size.coerceAtLeast(1))
            val slot=width/days.size.coerceAtLeast(1)
            Box {
                Column(Modifier.horizontalScroll(scroll).width(width)) {
                    Box(Modifier.fillMaxWidth().height(plotHeight)) {
                        Canvas(Modifier.fillMaxSize()) {
                            for(i in 0..3)if(maximum>0||i==0) {
                                val y=(size.height-i/3f*size.height).coerceAtMost(size.height-.5f)
                                drawLine(colors.onSurfaceVariant.copy(alpha=if(i==0).25f else .12f),Offset(0f,y),Offset(size.width,y),strokeWidth=1.dp.toPx(),pathEffect=PathEffect.dashPathEffect(floatArrayOf(4.dp.toPx(),4.dp.toPx())))
                            }
                            val cell=size.width/days.size.coerceAtLeast(1)
                            days.forEachIndexed {i,day->if(day.milliseconds>0uL) {
                                val height=(day.milliseconds.toDouble()/ceiling*size.height).toFloat().coerceAtLeast(1f)
                                val bar=min(cell*.6f,48.dp.toPx())
                                drawRoundRect(colors.primary,Offset((i+.5f)*cell-bar/2,size.height-height),Size(bar,height),CornerRadius(3.dp.toPx()))
                            }}
                        }
                        Row(Modifier.fillMaxSize()) {
                            days.forEachIndexed {index,day->
                                val description=day.day.replace('-','/')+" · "+durationLabel(day.milliseconds)
                                Box(Modifier.width(slot).fillMaxHeight().semantics {contentDescription=description}.clickable {selected=index})
                            }
                        }
                    }
                    Row(Modifier.fillMaxWidth().heightIn(min=maxOf(26.dp,labelHeight+12.dp))) {
                        labels.forEach {label->Text(label,Modifier.width(slot).padding(top=8.dp),style=style,textAlign=TextAlign.Center,maxLines=1)}
                    }
                }
                if(maximum==0.0)Box(Modifier.fillMaxWidth().height(plotHeight),contentAlignment=Alignment.Center) {
                    Text(tr("暂无阅读记录","No reading records"),style=MaterialTheme.typography.bodySmall,color=colors.onSurfaceVariant)
                }
                selected?.let {index->
                    val day=days.getOrNull(index)
                    if(day!=null) {
                        val anchor=with(density){((index+.5f)*slot.toPx()-scroll.value).toDp()}.coerceIn(0.dp,maxOf(0.dp,viewportWidth-1.dp))
                        Box(Modifier.offset(x=anchor).width(1.dp)) {
                            DropdownMenu(expanded=true,onDismissRequest={selected=null},shape=RoundedCornerShape(8.dp),containerColor=colors.surfaceContainerHigh) {
                                Column(Modifier.padding(horizontal=12.dp,vertical=4.dp)) {
                                    Text(when(period){"year"->day.day.take(7);"all"->day.day.take(4);else->day.day}.replace('-','/'),style=MaterialTheme.typography.bodySmall)
                                    Spacer(Modifier.height(6.dp));StatisticsTime(day.milliseconds,22,fit=false)
                                }
                            }
                        }
                    }
                }
            }
        }
        Spacer(Modifier.width(10.dp))
        Box(Modifier.width(axisWidth).height(plotHeight)) {
            for(i in 0..3)if(maximum>0||i==0)Text(ticks[i],Modifier.offset(y=plotHeight*(1-i/3f)-labelHeight/2),style=style,maxLines=1)
        }
    }
}

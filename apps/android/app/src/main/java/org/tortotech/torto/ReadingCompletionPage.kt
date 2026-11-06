package org.tortotech.torto

import androidx.compose.animation.core.CubicBezierEasing
import androidx.compose.animation.core.animate
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.util.VelocityTracker
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import org.tortotech.torto.core.DayReading
import org.tortotech.torto.core.pageTurnCommit
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter

/** A synthetic page after the book; the underlying native position stays intact. */
@Composable internal fun ReadingCompletionPage(state:ScreenState,returnToBook:()->Unit,exit:()->Unit,retry:()->Unit,markFinished:()->Unit){
    val density=LocalDensity.current.density
    val scope=rememberCoroutineScope()
    var drag by remember(state.active?.id){mutableFloatStateOf(0f)}
    var settle by remember {mutableStateOf<Job?>(null)}
    val summary=state.completion
    val detail=summary?.statistics
    val finished=detail?.status=="finished"
    val pageLabel=tr("读完页","Finished reading page")
    BoxWithConstraints(Modifier.fillMaxSize()){
        val width=constraints.maxWidth.toFloat()
        fun finish(velocity:Float,cancelled:Boolean=false){
            val back=!cancelled&&drag>0&&pageTurnCommit(drag/density,width/density,velocity/density)
            settle?.cancel()
            settle=scope.launch {
                animate(drag,if(back)width else 0f,animationSpec=tween(140,easing=CubicBezierEasing(.215f,.61f,.355f,1f))){value,_->drag=value}
                if(back)returnToBook()
            }
        }
        Box(Modifier.fillMaxSize().pointerInput(state.active?.id,width){
            var velocity=VelocityTracker()
            detectHorizontalDragGestures(
                onDragStart={settle?.cancel();velocity=VelocityTracker()},
                onHorizontalDrag={change,amount->change.consume();velocity.addPosition(change.uptimeMillis,change.position);drag=(drag+amount).coerceIn(0f,width)},
                onDragEnd={finish(velocity.calculateVelocity().x)},onDragCancel={finish(0f,true)})
        }){
            Surface(Modifier.fillMaxSize().graphicsLayer {translationX=drag}.semantics {contentDescription=pageLabel},color=MaterialTheme.colorScheme.background){
                Column(Modifier.fillMaxSize().safeDrawingPadding()){
                    Column(Modifier.weight(1f).fillMaxWidth().verticalScroll(rememberScrollState()).padding(horizontal=16.dp,vertical=28.dp),horizontalAlignment=Alignment.CenterHorizontally){
                        Text(if(finished)tr("已读完","Finished")else tr("全书完","The End"),fontSize=32.sp,fontWeight=FontWeight.SemiBold,textAlign=TextAlign.Center)
                        Spacer(Modifier.height(24.dp))
                        Box(Modifier.size(172.dp).background(if(finished)Brush.verticalGradient(listOf(MaterialTheme.colorScheme.primary,MaterialTheme.colorScheme.primary.copy(alpha=.8f)))else Brush.verticalGradient(listOf(MaterialTheme.colorScheme.surfaceContainerLowest,MaterialTheme.colorScheme.surfaceContainerLowest)),CircleShape),contentAlignment=Alignment.Center){
                            if(finished)Column(horizontalAlignment=Alignment.CenterHorizontally){
                                Text(tr("累计读完","Books finished"),fontSize=14.sp,color=MaterialTheme.colorScheme.onPrimary)
                                Row(verticalAlignment=Alignment.Bottom,horizontalArrangement=Arrangement.Center){
                                    Text(summary?.finishedBooks?.toString() ?: "—",fontSize=56.sp,lineHeight=64.sp,fontWeight=FontWeight.Bold,color=MaterialTheme.colorScheme.onPrimary)
                                    if(tr("zh","en")=="zh")Text("本",Modifier.padding(bottom=8.dp),fontSize=22.sp,color=MaterialTheme.colorScheme.onPrimary)
                                }
                            }
                            else Text(tr("完","END"),fontSize=if(tr("zh","en")=="zh")104.sp else 46.sp,fontWeight=FontWeight.Bold,color=MaterialTheme.colorScheme.primary)
                        }
                        if(!finished)TextButton(onClick=markFinished,enabled=!state.completionLoading&&detail!=null,modifier=Modifier.padding(top=12.dp)){
                            Text(tr("标记读完","Mark finished"),fontSize=16.sp)
                        }
                        Spacer(Modifier.height(if(finished)32.dp else 20.dp))
                        SettingsGroup {
                            Column(Modifier.fillMaxWidth().padding(20.dp),verticalArrangement=Arrangement.spacedBy(28.dp)){
                                Row(horizontalArrangement=Arrangement.spacedBy(24.dp)){
                                    CompletionMetric(tr("累计时长","Total reading time"),Modifier.weight(1f)){
                                        if(detail!=null)StatisticsTime(detail.total,size=28)else CompletionPlaceholder()
                                        CompletionCaption(detail?.started?.let {tr("${completionDate(it)}开始阅读","Started ${completionDate(it)}")}.orEmpty())
                                    }
                                    CompletionMetric(tr("阅读天数","Reading days"),Modifier.weight(1f)){
                                        if(detail!=null)StatisticsValue(listOf(detail.days.toString() to tr("天","days")))else CompletionPlaceholder()
                                        CompletionCaption(detail?.last?.let {tr("上次阅读：${completionDate(it)}","Last read: ${completionDate(it)}")}.orEmpty())
                                    }
                                }
                                Row(horizontalArrangement=Arrangement.spacedBy(24.dp)){
                                    CompletionMetric(tr("想法笔记","Notes and highlights"),Modifier.weight(1f)){
                                        if(detail!=null)StatisticsValue(listOf(detail.highlights.toString() to tr("条","notes")))else CompletionPlaceholder()
                                        CompletionCaption(detail?.let {tr("${it.highlights-it.notes} 处高亮 · ${it.notes} 条批注","${it.highlights-it.notes} highlights · ${it.notes} comments")}.orEmpty())
                                    }
                                    CompletionMetric(tr("阅读明细","Reading history"),Modifier.weight(1f)){
                                        if(detail!=null)CompletionHistory(detail.distribution)else CompletionPlaceholder()
                                    }
                                }
                                state.completionError?.let {message->
                                    Text(message,color=MaterialTheme.colorScheme.error,style=MaterialTheme.typography.bodySmall)
                                    TextButton(onClick=retry,enabled=!state.completionLoading){Text(tr("重新加载统计","Reload statistics"))}
                                }
                            }
                        }
                    }
                    HorizontalDivider(thickness=.5.dp,color=MaterialTheme.colorScheme.outlineVariant.copy(alpha=.35f))
                    Box(Modifier.fillMaxWidth().background(MaterialTheme.colorScheme.surface).padding(16.dp)){
                        Button(onClick=exit,modifier=Modifier.fillMaxWidth().heightIn(min=48.dp),shape=RoundedCornerShape(14.dp)){
                            Text(tr("退出阅读","Exit reading"),fontSize=16.sp)
                        }
                    }
                }
            }
        }
    }
}

@Composable private fun CompletionMetric(title:String,modifier:Modifier,content:@Composable ColumnScope.()->Unit){
    Column(modifier,verticalArrangement=Arrangement.spacedBy(6.dp)){
        Text(title,fontSize=13.sp,color=MaterialTheme.colorScheme.onSurfaceVariant)
        content()
    }
}
@Composable private fun CompletionPlaceholder(){Text("—",fontSize=28.sp,color=MaterialTheme.colorScheme.onSurfaceVariant)}
@Composable private fun CompletionCaption(text:String){
    if(text.isNotBlank())Text(text,fontSize=11.sp,color=MaterialTheme.colorScheme.onSurfaceVariant,maxLines=2,overflow=TextOverflow.Ellipsis)
}
@Composable private fun CompletionHistory(distribution:List<DayReading>){
    val days=remember(distribution){distribution.filter {it.milliseconds>0uL}.sortedByDescending {it.day}.take(2)}
    val maximum=days.maxOfOrNull {it.milliseconds}?.toFloat()?.coerceAtLeast(1f) ?: 1f
    if(days.isEmpty())CompletionCaption(tr("暂无阅读明细","No reading history yet"))
    else Column(verticalArrangement=Arrangement.spacedBy(5.dp)){
        days.forEach {day->
            Column{
                Text(day.day,fontSize=10.sp,color=MaterialTheme.colorScheme.onSurfaceVariant)
                Box(Modifier.fillMaxWidth().height(5.dp).background(MaterialTheme.colorScheme.primaryContainer,RoundedCornerShape(3.dp))){
                    Box(Modifier.fillMaxWidth(day.milliseconds.toFloat()/maximum).fillMaxHeight().background(MaterialTheme.colorScheme.primary,RoundedCornerShape(3.dp)))
                }
            }
        }
    }
}
private fun completionDate(ms:ULong)=DateTimeFormatter.ofPattern("yyyy/MM/dd").format(Instant.ofEpochMilli(ms.toLong()).atZone(ZoneId.systemDefault()))

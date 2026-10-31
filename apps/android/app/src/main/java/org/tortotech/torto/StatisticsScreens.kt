package org.tortotech.torto

import android.graphics.Bitmap
import androidx.compose.foundation.*
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import org.tortotech.torto.core.StatisticsPeriod

/** Flat, outlined cards and mixed-size numbers follow the original app. */
@Composable private fun StatisticsCard(modifier:Modifier=Modifier,content:@Composable ColumnScope.()->Unit) {
    Card(modifier.fillMaxWidth(),shape=RoundedCornerShape(12.dp),
        colors=CardDefaults.cardColors(containerColor=MaterialTheme.colorScheme.surface),
        border=BorderStroke(1.dp,MaterialTheme.colorScheme.outlineVariant.copy(alpha=.6f)),
        elevation=CardDefaults.cardElevation(0.dp)) {
        Column(Modifier.padding(16.dp),content=content)
    }
}
@Composable private fun StatisticsGroupItem(first:Boolean=false,last:Boolean=false,vertical:Int=16,onClick:(()->Unit)?=null,content:@Composable ColumnScope.()->Unit) {
    val colors=MaterialTheme.colorScheme
    val shape=RoundedCornerShape(topStart=if(first)12.dp else 0.dp,topEnd=if(first)12.dp else 0.dp,bottomStart=if(last)12.dp else 0.dp,bottomEnd=if(last)12.dp else 0.dp)
    Column(Modifier.fillMaxWidth().clip(shape).background(colors.surface).drawWithContent {
        drawContent()
        val inset=.5.dp.toPx();val left=inset;val right=size.width-inset;val bottom=size.height-inset;val r=12.dp.toPx()
        val path=Path().apply {
            moveTo(left,if(first)r else 0f);lineTo(left,if(last)bottom-r else size.height)
            if(last){quadraticTo(left,bottom,left+r,bottom);lineTo(right-r,bottom);quadraticTo(right,bottom,right,bottom-r)}
            else moveTo(right,size.height)
            lineTo(right,if(first)r else 0f)
            if(first){quadraticTo(right,inset,right-r,inset);lineTo(left+r,inset);quadraticTo(left,inset,left,r)}
        }
        drawPath(path,colors.outlineVariant.copy(alpha=.6f),style=Stroke(1.dp.toPx()))
    }.then(if(onClick!=null)Modifier.clickable(onClick=onClick) else Modifier).padding(start=16.dp,end=16.dp,top=vertical.dp,bottom=if(first&&!last)0.dp else (vertical+if(last&&!first)16 else 0).dp),content=content)
}
@Composable internal fun StatisticsTitle(text:String) {
    Text(text,Modifier.padding(bottom=12.dp),style=MaterialTheme.typography.titleSmall,color=MaterialTheme.colorScheme.onSurfaceVariant)
}
@Composable internal fun StatisticsValue(parts:List<Pair<String,String>>,size:Int=28,fit:Boolean=true) {
    val secondary=MaterialTheme.colorScheme.onSurfaceVariant
    val measurer=rememberTextMeasurer()
    fun value(scale:Float)=buildAnnotatedString {
        for((number,unit) in parts){
            append(number)
            withStyle(SpanStyle(fontSize=(12*scale).sp,fontWeight=FontWeight.Normal,color=secondary)){append(" $unit ")}
        }
    }
    val style=MaterialTheme.typography.headlineMedium.copy(fontSize=size.sp,fontWeight=FontWeight.Medium,fontFeatureSettings="tnum")
    if(!fit){Text(value(1f),style=style,maxLines=1,softWrap=false);return}
    BoxWithConstraints {
        val measured=measurer.measure(value(1f),style,softWrap=false,maxLines=1)
        val scale=(constraints.maxWidth.toFloat()/measured.size.width.coerceAtLeast(1)).coerceAtMost(1f)
        Text(value(scale),style=style.copy(fontSize=(size*scale).sp),maxLines=1,softWrap=false)
    }
}
@Composable internal fun StatisticsTime(ms:ULong,size:Int=28,fit:Boolean=true) {
    val minutes=ms/60000uL
    StatisticsValue(buildList {
        if(minutes>=60uL)add((minutes/60uL).toString() to tr("小时","h"))
        if(minutes<60uL||minutes%60uL!=0uL)add((if(ms>0uL&&minutes==0uL)"<1" else (minutes%60uL).toString()) to tr("分","min"))
    },size,fit)
}
@Composable private fun StatisticsMetric(label:String,modifier:Modifier=Modifier,value:@Composable ()->Unit) {
    Column(modifier){Text(label,style=MaterialTheme.typography.bodySmall);Spacer(Modifier.height(6.dp));value()}
}
@Composable private fun StatisticsCover(id:String,model:ReaderModel,width:Int=44) {
    val cover by produceState<Bitmap?>(null,id){value=model.cover(id)}
    Box(Modifier.width(width.dp).height((width*1.4f).dp).clip(RoundedCornerShape(5.dp)).background(MaterialTheme.colorScheme.surfaceContainerHighest),contentAlignment=Alignment.Center) {
        cover?.let {Image(it.asImageBitmap(),null,Modifier.fillMaxSize(),contentScale=ContentScale.Crop)}
            ?: AppIcon("book",tint=MaterialTheme.colorScheme.onSurfaceVariant)
    }
}
private fun statisticsDate(ms:ULong?):String=if(ms==null||ms==0uL)"—" else Instant.ofEpochMilli(ms.toLong()).atZone(ZoneId.systemDefault()).toLocalDate().toString().replace('-','/')
@Composable private fun statisticsRange(stats:StatisticsPeriod):String {
    if(stats.period=="all")return ""
    if(stats.period=="year")return stats.start.take(4)
    if(stats.period=="month")return stats.start.take(7).replace('-','/')
    val start=stats.start;val end=stats.end
    val left=if(start.take(4)==LocalDate.now().year.toString())start.takeLast(5) else start
    val right=if(start.take(4)==end.take(4))end.takeLast(5) else end
    return left.replace('-','/')+tr(" 至 "," – ")+right.replace('-','/')
}

@OptIn(ExperimentalMaterial3Api::class,ExperimentalLayoutApi::class)
@Composable fun StatisticsScreen(state:ScreenState,model:ReaderModel,detail:(String)->Unit) {
    LaunchedEffect(Unit){model.loadStatisticsPeriod(state.statsKind,state.statsOffset)}
    val stats=state.statsPeriod?.takeIf {it.period==state.statsKind&&it.offset==state.statsOffset}
    val list=androidx.compose.foundation.lazy.rememberLazyListState()
    LazyColumn(state=list,contentPadding=PaddingValues(16.dp),verticalArrangement=Arrangement.Top) {
        item {
            val periods=listOf("week" to tr("周","Week"),"month" to tr("月","Month"),"year" to tr("年","Year"),"all" to tr("总","All"))
            SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth().padding(bottom=8.dp)) {
                periods.forEachIndexed {index,(id,label)->SegmentedButton(
                    selected=state.statsKind==id,onClick={model.loadStatisticsPeriod(id,0)},
                    shape=SegmentedButtonDefaults.itemShape(index,periods.size),icon={},enabled=!state.statsLoading) {Text(label,maxLines=1)} }
            }
            if(state.statsKind!="all")FlowRow(Modifier.fillMaxWidth().padding(bottom=8.dp)) {
                IconButton(onClick={model.loadStatisticsPeriod(state.statsKind,state.statsOffset-1)},enabled=!state.statsLoading&&state.statsOffset> -200){AppIcon("previous",tr("上一周期","Previous period"))}
                Text(stats?.let {statisticsRange(it)}.orEmpty(),Modifier.padding(vertical=14.dp),style=MaterialTheme.typography.bodyMedium,color=MaterialTheme.colorScheme.onSurfaceVariant)
                ActionIcon("next",tr("下一周期","Next period"),!state.statsLoading&&state.statsOffset<0){model.loadStatisticsPeriod(state.statsKind,state.statsOffset+1)}
                if(state.statsOffset<0)TextButton(onClick={model.loadStatisticsPeriod(state.statsKind,0)},enabled=!state.statsLoading) {
                    Text(when(state.statsKind){"month"->tr("本月","This month");"year"->tr("今年","This year");else->tr("本周","This week")})
                }
            }
        }
        if(state.statsLoading)item{LinearProgressIndicator(Modifier.fillMaxWidth().padding(bottom=12.dp))}
        if(stats!=null) {
            val ranked=stats.books.filter {it.milliseconds>0uL}
            item {Row(Modifier.padding(bottom=12.dp),horizontalArrangement=Arrangement.spacedBy(12.dp)) {
                StatisticsCard(Modifier.weight(1f)){StatisticsMetric(tr("阅读时长","Reading time")){StatisticsTime(stats.total)}}
                StatisticsCard(Modifier.weight(1f)){StatisticsMetric(tr("阅读","Reading days")){StatisticsValue(listOf(stats.days.toString() to tr("天","days")))}}
            }}
            item {Row(Modifier.padding(bottom=12.dp),horizontalArrangement=Arrangement.spacedBy(12.dp)) {
                StatisticsCard(Modifier.weight(1f)){StatisticsMetric(tr("读过","Books read")){StatisticsValue(listOf(ranked.size.toString() to tr("本","books")))}}
                StatisticsCard(Modifier.weight(1f)){StatisticsMetric(tr("读完","Finished")){StatisticsValue(listOf(stats.finished.toString() to tr("本","books")))}}
            }}
            item {StatisticsCard(Modifier.padding(bottom=12.dp)) {StatisticsTitle(tr("阅读时长分布","Reading time distribution"));ReadingDistribution(stats.distribution,state.statsKind,list.isScrollInProgress)}}
            item {StatisticsGroupItem(first=true,last=ranked.isEmpty()) {
                StatisticsTitle(tr("阅读最久","Most time spent reading"))
                if(ranked.isEmpty())Text(tr("这段时间还没有阅读记录","No reading recorded in this period."),Modifier.padding(vertical=12.dp),style=MaterialTheme.typography.bodySmall)
            }}
            itemsIndexed(ranked,key={_,book->book.id}){index,book->
                StatisticsGroupItem(last=index==ranked.lastIndex,vertical=10,onClick={detail(book.id)}) {
                    Row(verticalAlignment=Alignment.CenterVertically) {
                        StatisticsCover(book.id,model);Spacer(Modifier.width(14.dp))
                        Column(Modifier.weight(1f),verticalArrangement=Arrangement.spacedBy(5.dp)) {
                            StatisticsTime(book.milliseconds,20)
                            Text(book.title.ifEmpty {tr("未知书籍","Unknown book")},style=MaterialTheme.typography.bodySmall,color=MaterialTheme.colorScheme.onSurfaceVariant,maxLines=1,overflow=TextOverflow.Ellipsis)
                        }
                        Spacer(Modifier.width(8.dp));AppIcon("next",modifier=Modifier.size(18.dp),tint=MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                }
            }
        }
    }
}

@Composable fun BookStatisticsScreen(state:ScreenState,model:ReaderModel) {
    LaunchedEffect(state.statsBookId){state.statsBookId?.let {model.loadBookStatistics(it)}}
    val detail=state.bookStatistics?.takeIf {it.id==state.statsBookId}
    if(detail==null){if(state.statsLoading)Box(Modifier.fillMaxSize(),contentAlignment=Alignment.Center){CircularProgressIndicator()};return}
    val days=remember(detail.distribution){detail.distribution.filter {it.milliseconds>=60000uL}.sortedByDescending {it.day}}
    val measurer=rememberTextMeasurer()
    val textStyle=MaterialTheme.typography.bodyMedium.copy(fontFeatureSettings="tnum")
    val density=androidx.compose.ui.platform.LocalDensity.current
    val hourWidth=with(density){measurer.measure(days.maxByOrNull {(it.milliseconds/3600000uL).toString().length}?.let {(it.milliseconds/3600000uL).toString()} ?: "0",textStyle).size.width.toDp()}
    val minuteWidth=with(density){measurer.measure("00",textStyle).size.width.toDp()}
    LazyColumn(contentPadding=PaddingValues(16.dp)) {
        item {StatisticsCard(Modifier.padding(bottom=12.dp)) {Row(verticalAlignment=Alignment.Top) {
            StatisticsCover(detail.id,model,76);Spacer(Modifier.width(16.dp))
            Column(Modifier.weight(1f)) {
                Text(detail.title,style=MaterialTheme.typography.titleMedium.copy(fontSize=18.sp),maxLines=2,overflow=TextOverflow.Ellipsis)
                Text(detail.authors,style=MaterialTheme.typography.bodyMedium)
                Spacer(Modifier.height(12.dp))
                Row(verticalAlignment=Alignment.CenterVertically,horizontalArrangement=Arrangement.spacedBy(10.dp)) {
                    Text(when(detail.status){"finished"->tr("已读完","Finished");"reading"->tr("在读","Reading");else->tr("未开始","Not started")},color=MaterialTheme.colorScheme.primary,style=MaterialTheme.typography.bodyMedium)
                    if(detail.status=="reading")Text(String.format(java.util.Locale.ROOT,"%.1f%%",detail.progress.coerceIn(0.0,1.0)*100),color=MaterialTheme.colorScheme.onSurfaceVariant,style=MaterialTheme.typography.bodyMedium)
                }
            }
        }}}
        item {StatisticsCard(Modifier.padding(bottom=12.dp)) {
            Row(horizontalArrangement=Arrangement.spacedBy(12.dp)) {
                StatisticsMetric(tr("累计阅读","Total reading"),Modifier.weight(1f)){StatisticsTime(detail.total)}
                StatisticsMetric(tr("阅读天数","Reading days"),Modifier.weight(1f)){StatisticsValue(listOf(detail.days.toString() to tr("天","days")))}
            }
            Text("${detail.highlights} "+tr("处高亮","highlights")+" · ${detail.notes} "+tr("条批注","notes"),Modifier.padding(top=12.dp),style=MaterialTheme.typography.bodySmall)
        }}
        item {StatisticsCard(Modifier.padding(bottom=12.dp)) {
            for((label,value)in listOf(tr("加入书架","Added") to statisticsDate(detail.added),tr("开始阅读","Started") to statisticsDate(detail.started),tr("最近阅读","Last read") to statisticsDate(detail.last),tr("读完日期","Finished") to (detail.finished?.replace('-','/') ?: "—"))) {
                Row(Modifier.fillMaxWidth().padding(vertical=7.dp),verticalAlignment=Alignment.CenterVertically) {
                    Text(label,Modifier.weight(1f),style=MaterialTheme.typography.bodyMedium);Spacer(Modifier.width(12.dp))
                    Text(value,style=MaterialTheme.typography.bodyMedium.copy(fontFeatureSettings="tnum"))
                }
            }
        }}
        item {StatisticsGroupItem(first=true,last=days.isEmpty()) {
            StatisticsTitle(tr("阅读历史","Reading history"))
            if(days.isEmpty())Text(tr("暂无满 1 分钟的阅读记录","No days with at least one minute of reading."),style=MaterialTheme.typography.bodySmall)
        }}
        itemsIndexed(days,key={_,day->day.day}){index,day->
            StatisticsGroupItem(last=index==days.lastIndex,vertical=12) {
                Row(Modifier.fillMaxWidth(),verticalAlignment=Alignment.CenterVertically) {
                    Text(day.day.replace('-','/'),Modifier.weight(1f),style=MaterialTheme.typography.bodyMedium.copy(fontFeatureSettings="tnum"))
                    Text((day.milliseconds/3600000uL).toString(),Modifier.padding(start=12.dp).width(hourWidth),textAlign=androidx.compose.ui.text.style.TextAlign.End,style=MaterialTheme.typography.bodyMedium.copy(fontFeatureSettings="tnum"))
                    Text(tr("小时","h"),Modifier.padding(start=4.dp),style=MaterialTheme.typography.bodyMedium)
                    Text((day.milliseconds/60000uL%60uL).toString(),Modifier.padding(start=8.dp).width(minuteWidth),textAlign=androidx.compose.ui.text.style.TextAlign.End,style=MaterialTheme.typography.bodyMedium.copy(fontFeatureSettings="tnum"))
                    Text(tr("分","min"),Modifier.padding(start=4.dp),style=MaterialTheme.typography.bodyMedium)
                }
            }
        }
        state.books.firstOrNull {it.id==detail.id}?.let {book->item{TextButton(onClick={model.open(book)}){Text(tr("继续阅读","Continue reading"))}}}
    }
}

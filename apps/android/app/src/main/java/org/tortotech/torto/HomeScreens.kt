package org.tortotech.torto

import android.graphics.Bitmap
import androidx.compose.foundation.*
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.grid.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import org.tortotech.torto.core.*

@OptIn(ExperimentalFoundationApi::class)
@Composable fun ShelfScreen(state: ScreenState,model: ReaderModel,onActions:(BookSummary)->Unit) {
    if(!state.loaded) { Box(Modifier.fillMaxSize(),contentAlignment=Alignment.Center){CircularProgressIndicator()};return }
    if(state.books.isEmpty()) { Box(Modifier.fillMaxSize().padding(24.dp),contentAlignment=Alignment.Center) {
        Text(tr("书架还是空的。\n点击 + 导入 EPUB、FB2、CBZ、\nMOBI、CHM 或 PDF。","No books yet.\nTap + to import an EPUB, FB2, CBZ,\nMOBI, CHM or PDF."),textAlign=TextAlign.Center)
    };return }
    LazyVerticalGrid(columns=GridCells.Fixed(3),contentPadding=PaddingValues(start=18.dp,end=18.dp,top=20.dp,bottom=32.dp),horizontalArrangement=Arrangement.spacedBy(14.dp),verticalArrangement=Arrangement.spacedBy(22.dp)) {
        items(state.books,key={it.id}) { book ->
            val cover by produceState<Bitmap?>(model.cachedCover(book.id),book.id,state.cloudStatus.coverRevision){value=model.cover(book.id)}
            Column(Modifier.combinedClickable(onClick={model.open(book)},onLongClick={onActions(book)})) {
                Box(Modifier.fillMaxWidth().aspectRatio(.7f).shadow(3.dp,RoundedCornerShape(6.dp)).clip(RoundedCornerShape(6.dp)).background(MaterialTheme.colorScheme.surfaceContainerHighest),contentAlignment=Alignment.Center) {
                    cover?.let { Image(it.asImageBitmap(),null,Modifier.fillMaxSize(),contentScale=ContentScale.Crop) } ?: AppIcon("book",size=32.dp,tint=MaterialTheme.colorScheme.onSurfaceVariant)
                }
                Spacer(Modifier.height(8.dp));Text(book.title,style=MaterialTheme.typography.titleSmall,fontSize=12.sp,maxLines=1,overflow=TextOverflow.Ellipsis)
            }
        }
    }
}
@Composable fun SettingsRow(icon:String,title:String,subtitle:String?=null,onClick:()->Unit) {
    SettingsItem(title,description=subtitle,icon=icon,onClick=onClick)
}
@Composable fun SectionLabel(text:String) {Text(text,Modifier.padding(start=16.dp,top=4.dp),color=MaterialTheme.colorScheme.onSurfaceVariant,fontSize=13.sp)}
@Composable fun durationLabel(ms: ULong): String {
    val minutes = (ms / 60000uL).toLong()
    return when {
        ms > 0uL && minutes == 0L -> tr("不到 1 分钟", "<1 min")
        minutes < 60 -> tr("$minutes 分钟", "$minutes min")
        else -> tr("${minutes / 60} 小时 ${minutes % 60} 分钟", "${minutes / 60} h ${minutes % 60} min")
    }
}

@Composable fun ProfileScreen(state:ScreenState,onStatistics:()->Unit) {
    LazyColumn(contentPadding=PaddingValues(vertical=12.dp)) {
        item { Card(onClick=onStatistics,modifier=Modifier.padding(start=16.dp,end=16.dp,top=4.dp,bottom=12.dp),colors=CardDefaults.cardColors(containerColor=MaterialTheme.colorScheme.surface)) {
            Row(Modifier.fillMaxWidth().padding(20.dp),verticalAlignment=Alignment.CenterVertically){Column(Modifier.weight(1f)) {
                Text(tr("阅读统计","Reading statistics"),style=MaterialTheme.typography.titleMedium)
                Spacer(Modifier.height(12.dp));Text(state.statistics?.let { durationLabel(it.today) } ?: "—",style=MaterialTheme.typography.headlineMedium)
                Text(tr("今日阅读","Today"));Spacer(Modifier.height(8.dp))
                Text(tr("本周","This week")+" ${state.statistics?.let { durationLabel(it.week) } ?: "—"} · ${state.statistics?.days ?: 0u} "+tr("天","days"),style=MaterialTheme.typography.bodyMedium)
            };AppIcon("next")}
        } }
    }
}
@Composable fun SettingsScreen(open:(String)->Unit) {
    LazyColumn(contentPadding=PaddingValues(16.dp),verticalArrangement=Arrangement.spacedBy(12.dp)) {
        item { SettingsGroup {
            SettingsRow("settings",tr("系统","System")){open("system")}
            SettingsRow("type",tr("排版","Typesetting")){open("typography")}
        } }
        item { SettingsGroup {
            SettingsRow("ai",tr("AI 提供商","AI providers")){open("ai")}
            SettingsRow("translate",tr("翻译","Translation")){open("translation")}
            SettingsRow("chat",tr("阅读助手","Reading assistant")){open("assistant")}
        } }
        item { SettingsGroup {
            SettingsRow("cloud",tr("云同步","Cloud sync")){open("sync")}
            SettingsRow("info",tr("关于 Torto","About Torto")){open("about")}
        } }
    }
}

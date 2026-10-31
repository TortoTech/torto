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
    val recent=state.books.filter { it.lastRead>0uL }.maxByOrNull { it.lastRead }?.id
    LazyVerticalGrid(columns=GridCells.Fixed(3),contentPadding=PaddingValues(start=18.dp,end=18.dp,top=20.dp,bottom=32.dp),horizontalArrangement=Arrangement.spacedBy(14.dp),verticalArrangement=Arrangement.spacedBy(22.dp)) {
        items(state.books,key={it.id}) { book ->
            val cover by produceState<Bitmap?>(null,book.id){value=model.cover(book.id)}
            Column(Modifier.combinedClickable(onClick={model.open(book)},onLongClick={onActions(book)})) {
                Box(Modifier.fillMaxWidth().aspectRatio(.7f).shadow(3.dp,RoundedCornerShape(6.dp)).clip(RoundedCornerShape(6.dp)).background(MaterialTheme.colorScheme.surfaceContainerHighest).then(if(book.id==recent)Modifier.border(3.dp,MaterialTheme.colorScheme.primary,RoundedCornerShape(6.dp)) else Modifier),contentAlignment=Alignment.Center) {
                    cover?.let { Image(it.asImageBitmap(),null,Modifier.fillMaxSize(),contentScale=ContentScale.Crop) } ?: AppIcon("book",modifier=Modifier.size(32.dp),tint=MaterialTheme.colorScheme.onSurfaceVariant)
                    if(book.id==recent) Text(tr("继续阅读","Continue reading"),Modifier.align(Alignment.BottomCenter).fillMaxWidth().background(MaterialTheme.colorScheme.primary).padding(vertical=4.dp),color=MaterialTheme.colorScheme.onPrimary,fontSize=12.sp,textAlign=TextAlign.Center)
                }
                Spacer(Modifier.height(8.dp));Text(book.title,style=MaterialTheme.typography.titleSmall,maxLines=1,overflow=TextOverflow.Ellipsis)
            }
        }
    }
}
@Composable fun SettingsRow(icon:String,title:String,subtitle:String?=null,onClick:()->Unit) {
    ListItem(headlineContent={Text(title)},supportingContent=subtitle?.let { {Text(it)} },leadingContent={AppIcon(icon)},trailingContent={AppIcon("next")},modifier=Modifier.clickable(onClick=onClick),colors=ListItemDefaults.colors(containerColor=MaterialTheme.colorScheme.background))
}
@Composable fun SectionLabel(text:String) {Text(text,Modifier.padding(start=16.dp,top=20.dp,bottom=6.dp),color=MaterialTheme.colorScheme.primary,style=MaterialTheme.typography.labelLarge)}
@Composable fun durationLabel(ms: ULong): String {
    val minutes = (ms / 60000uL).toLong()
    return when {
        ms > 0uL && minutes == 0L -> tr("不到 1 分钟", "<1 min")
        minutes < 60 -> tr("$minutes 分钟", "$minutes min")
        else -> tr("${minutes / 60} 小时 ${minutes % 60} 分钟", "${minutes / 60} h ${minutes % 60} min")
    }
}

@Composable fun ProfileScreen(state:ScreenState,onSettings:()->Unit,onStatistics:()->Unit) {
    LazyColumn(contentPadding=PaddingValues(vertical=12.dp)) {
        item { Card(onClick=onStatistics,modifier=Modifier.padding(start=16.dp,end=16.dp,top=4.dp,bottom=12.dp),colors=CardDefaults.cardColors(containerColor=MaterialTheme.colorScheme.surfaceContainerLow)) {
            Row(Modifier.fillMaxWidth().padding(20.dp),verticalAlignment=Alignment.CenterVertically){Column(Modifier.weight(1f)) {
                Text(tr("阅读统计","Reading statistics"),style=MaterialTheme.typography.titleMedium)
                Spacer(Modifier.height(12.dp));Text(state.statistics?.let { durationLabel(it.today) } ?: "—",style=MaterialTheme.typography.headlineMedium)
                Text(tr("今日阅读","Today"));Spacer(Modifier.height(8.dp))
                Text(tr("本周","This week")+" ${state.statistics?.let { durationLabel(it.week) } ?: "—"} · ${state.statistics?.days ?: 0u} "+tr("天","days"),style=MaterialTheme.typography.bodyMedium)
            };AppIcon("next")}
        } }
        item { SettingsRow("settings",tr("设置","Settings"),onClick=onSettings) }
    }
}
@Composable fun SettingsScreen(open:(String)->Unit) {
    LazyColumn(contentPadding=PaddingValues(bottom=24.dp)) {
        item { SectionLabel(tr("通用","General")) }
        item { SettingsRow("settings",tr("系统","System"),tr("主题与界面语言","Theme and interface language")){open("system")} }
        item { SettingsRow("type",tr("排版","Typesetting"),tr("正文样式、字号与阅读字体","Content style, font size and reading fonts")){open("typography")} }
        item { SectionLabel(tr("智能功能","Intelligence")) }
        item { SettingsRow("ai",tr("AI 提供商","AI providers"),tr("服务地址、密钥与模型","Endpoint, API keys and models")){open("ai")} }
        item { SettingsRow("ai",tr("AI 排版","AI layout"),tr("自动处理与排版模型","Automatic processing and layout model")){open("layout")} }
        item { SettingsRow("translate",tr("翻译","Translation"),tr("模型与目标语言","Model and target language")){open("translation")} }
        item { SettingsRow("chat",tr("阅读助手","Reading assistant"),tr("对话模型","Chat model")){open("assistant")} }
        item { SectionLabel(tr("数据","Data")) }
        item { SettingsRow("cloud",tr("云同步","Cloud sync"),tr("WebDAV 书籍与阅读进度","WebDAV books and reading progress")){open("sync")} }
        item { SectionLabel(tr("其他","Other")) }
        item { SettingsRow("info",tr("关于 Torto","About Torto")){open("about")} }
    }
}
@Composable fun SystemScreen(state:ScreenState,model:ReaderModel) {
    Column(Modifier.verticalScroll(rememberScrollState()).padding(vertical=12.dp)) {
        SectionLabel(tr("主题","Theme"))
        for((value,label) in listOf("system" to tr("跟随系统","System"),"light" to tr("浅色","Light"),"dark" to tr("深色","Dark"))) {
            ListItem(headlineContent={Text(label)},trailingContent={RadioButton(state.appTheme==value,{model.interfaceSettings(value,state.language)})},modifier=Modifier.clickable {model.interfaceSettings(value,state.language)},colors=ListItemDefaults.colors(containerColor=MaterialTheme.colorScheme.background))
        }
        SectionLabel(tr("界面语言","Interface language"))
        for((value,label) in listOf("system" to tr("跟随系统","System"),"zh-CN" to "简体中文","en" to "English")) {
            ListItem(headlineContent={Text(label)},trailingContent={RadioButton(state.language==value,{model.interfaceSettings(state.appTheme,value)})},modifier=Modifier.clickable {model.interfaceSettings(state.appTheme,value)},colors=ListItemDefaults.colors(containerColor=MaterialTheme.colorScheme.background))
        }
    }
}

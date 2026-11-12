package org.tortotech.torto

import androidx.compose.foundation.*
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.semantics.*
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import org.tortotech.torto.core.ReadingOptions

internal enum class ReadingFontSetting { Cjk, CjkLatin, Latin, LatinCjk, Code }

private data class FontChoice(val title:String,val pageTitle:String,val value:String,val options:List<Pair<String,String>>)

@Composable private fun fontChoice(setting:ReadingFontSetting,state:ScreenState):FontChoice {
    val reading=state.reading
    val fonts=state.fontFamilies
    val cjk=fonts?.cjk.orEmpty().map {it to it}
    val latin=fonts?.latin.orEmpty().map {it to it}
    return when(setting){
        ReadingFontSetting.Cjk->FontChoice(tr("主要字体","Primary font"),tr("中日韩主要字体","CJK primary font"),reading?.cjkFont.orEmpty(),cjk)
        ReadingFontSetting.CjkLatin->FontChoice(tr("拉丁字体","Latin font"),tr("拉丁字体","Latin font"),reading?.cjkLatinFont.orEmpty(),listOf("" to tr("跟随其他语言字体","Use other-language font"))+latin)
        ReadingFontSetting.Latin->FontChoice(tr("主要字体","Primary font"),tr("其他语言主要字体","Other-language primary font"),reading?.latinFont.orEmpty(),latin)
        ReadingFontSetting.LatinCjk->FontChoice(tr("中日韩字体","CJK font"),tr("中日韩字体","CJK font"),reading?.latinCjkFont.orEmpty(),listOf("" to tr("跟随中日韩主要字体","Use CJK primary font"))+cjk)
        ReadingFontSetting.Code->FontChoice(tr("代码字体","Code font"),tr("代码字体","Code font"),reading?.monospaceFont.orEmpty(),fonts?.monospace.orEmpty().map {it to it})
    }
}

private fun ReadingOptions.withFont(setting:ReadingFontSetting,value:String)=when(setting){
    ReadingFontSetting.Cjk->copy(cjkFont=value)
    ReadingFontSetting.CjkLatin->copy(cjkLatinFont=value)
    ReadingFontSetting.Latin->copy(latinFont=value)
    ReadingFontSetting.LatinCjk->copy(latinCjkFont=value)
    ReadingFontSetting.Code->copy(monospaceFont=value)
}

/** Both settings routes use the same font fields and persistence. Only navigation differs. */
@Composable internal fun TypographyFontGroups(state:ScreenState,model:ReaderModel,openFont:((ReadingFontSetting)->Unit)?=null,general:@Composable ColumnScope.()->Unit={}) {
    LaunchedEffect(model){model.loadFonts()}
    @Composable fun choice(setting:ReadingFontSetting){
        val spec=fontChoice(setting,state)
        val enabled=!state.busy&&state.fontFamilies!=null&&spec.options.isNotEmpty()
        if(openFont==null)SettingChoice(spec.title,spec.value,spec.options,enabled,pageTitle=spec.pageTitle,searchable=true){value->state.reading?.let {model.saveReadingOptions(it.withFont(setting,value))}}
        else SettingsItem(spec.title,value=spec.options.firstOrNull {it.first==spec.value}?.second ?: spec.value,enabled=enabled){openFont(setting)}
    }
    if(state.reading!=null){
        SectionLabel(tr("中日韩书籍","CJK books"))
        SettingsGroup {choice(ReadingFontSetting.Cjk);choice(ReadingFontSetting.CjkLatin)}
        SectionLabel(tr("其他语言书籍","Other-language books"))
        SettingsGroup {choice(ReadingFontSetting.Latin);choice(ReadingFontSetting.LatinCjk)}
    }
    SectionLabel(tr("通用","General"))
    SettingsGroup {if(state.reading!=null)choice(ReadingFontSetting.Code);general()}
}

@Composable internal fun TypographyFontDownloads(state:ScreenState,model:ReaderModel){
    if(state.fontApplyAvailable||!state.fontsReady)SettingsGroup {
        if(state.fontApplyAvailable)SettingsItem(tr("应用已下载字体","Apply downloaded fonts"),enabled=!state.busy,onClick=model::applyDownloadedFonts)
        if(!state.fontsReady)SettingsItem(when {
            state.fontsDownloading->tr("默认字体下载中","Downloading reading fonts")
            state.fontsError!=null->tr("重试下载默认字体","Retry reading font download")
            else->tr("下载默认字体","Download reading fonts")
        },enabled=!state.fontsDownloading,onClick=model::downloadFonts)
    }
}

@Composable internal fun ReadingTypographyPanel(state:ScreenState,model:ReaderModel,open:(String)->Unit){
    var size by remember(state.fontSize){mutableFloatStateOf(readingFontSize(state.fontSize))}
    Column(Modifier.fillMaxWidth().padding(horizontal=20.dp,vertical=16.dp),verticalArrangement=Arrangement.spacedBy(16.dp)){
        ReadingFontSizeSlider(size,!state.busy,{size=readingFontSize(it)}){value->
            size=readingFontSize(value)
            val current=model.state.value
            if(size!=current.fontSize)model.configure(size,current.unified,current.dark)
        }
        Row(Modifier.fillMaxWidth(),horizontalArrangement=Arrangement.spacedBy(10.dp)){
            TypographyEntry(tr("字体样式","Font styles"),!state.busy,Modifier.weight(1f)){open("font-style")}
            TypographyEntry(tr("AI排版","AI layout"),!state.busy,Modifier.weight(1f)){open("layout-settings")}
            if(state.focusAvailable||state.focus)TypographyEntry(if(state.focus)tr("专注模式","Focus mode")else tr("经典模式","Classic mode"),!state.busy,Modifier.weight(1f)){open("reading-mode")}
        }
    }
}

@Composable private fun TypographyEntry(label:String,enabled:Boolean,modifier:Modifier,onClick:()->Unit){
    Box(modifier.height(48.dp).clickable(enabled=enabled,role=Role.Button,interactionSource=remember {androidx.compose.foundation.interaction.MutableInteractionSource()},indication=null,onClick=onClick),contentAlignment=Alignment.Center){
        Surface(Modifier.fillMaxWidth().height(36.dp),shape=CircleShape,color=if(MaterialTheme.colorScheme.background.luminance()<.5f)MaterialTheme.colorScheme.surfaceContainerHigh else Color(0xfff7f8f8)){
            Row(Modifier.fillMaxSize().padding(horizontal=8.dp),verticalAlignment=Alignment.CenterVertically,horizontalArrangement=Arrangement.Center){
                Text(label,Modifier.weight(1f,fill=false),fontSize=14.sp,lineHeight=20.sp,maxLines=1,overflow=TextOverflow.Ellipsis,color=MaterialTheme.colorScheme.onSurface.copy(alpha=if(enabled)1f else .38f))
                Spacer(Modifier.width(2.dp))
                AppIcon("next",size=12.dp,tint=defaultIconColor())
            }
        }
    }
}

@Composable internal fun ReadingFontSizeSlider(value:Float,enabled:Boolean,onChange:(Float)->Unit,onCommit:(Float)->Unit){
    val sizeLabel=tr("字号","Font size")
    val decrease=tr("减小字号","Decrease font size")
    val increase=tr("增大字号","Increase font size")
    ReadingCapsuleSlider(value,enabled,sizeLabel,readingFontSizeRange,steps=readingFontSizeSteps,thumbText=value.toInt().toString(),
        onChange={onChange(readingFontSize(it))},onCommit={onCommit(readingFontSize(it))},leading={
            Box(Modifier.width(40.dp).fillMaxHeight().semantics {contentDescription=decrease}.clickable(enabled=enabled&&value>16f,role=Role.Button,onClickLabel=decrease){onCommit(value-2f)},contentAlignment=Alignment.Center){Text("A",fontSize=12.sp,color=defaultIconColor())}
        },trailing={
            Box(Modifier.width(40.dp).fillMaxHeight().semantics {contentDescription=increase}.clickable(enabled=enabled&&value<40f,role=Role.Button,onClickLabel=increase){onCommit(value+2f)},contentAlignment=Alignment.Center){Text("A",fontSize=18.sp,color=defaultIconColor())}
        })
}

@Composable internal fun ReadingFontStylePanel(state:ScreenState,model:ReaderModel,back:()->Unit,choose:(ReadingFontSetting)->Unit){
    ReadingPanelPage(tr("字体样式","Font styles"),back,fill=false){
        Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(16.dp),verticalArrangement=Arrangement.spacedBy(12.dp)){
            TypographyFontGroups(state,model,choose)
            TypographyFontDownloads(state,model)
        }
    }
}

@Composable internal fun ReadingFontPicker(setting:ReadingFontSetting,state:ScreenState,model:ReaderModel,back:()->Unit){
    val spec=fontChoice(setting,state)
    var query by rememberSaveable(setting){mutableStateOf("")}
    val visible=remember(query,spec.options){if(query.isBlank())spec.options else spec.options.filter {it.second.contains(query,true)}}
    val list=rememberLazyListState()
    LaunchedEffect(query){list.scrollToItem(if(query.isBlank())(visible.indexOfFirst {it.first==spec.value}-2).coerceAtLeast(0)else 0)}
    val manager=LocalFocusManager.current;val keyboard=LocalSoftwareKeyboardController.current
    DisposableEffect(Unit){onDispose {manager.clearFocus();keyboard?.hide()}}
    ReadingPanelPage(spec.pageTitle,back,fill=false){
        Column(Modifier.fillMaxWidth().padding(horizontal=16.dp).padding(bottom=16.dp),verticalArrangement=Arrangement.spacedBy(12.dp)){
            TextField(query,{query=it},Modifier.fillMaxWidth(),singleLine=true,placeholder={Text(tr("搜索字体","Search fonts"))},leadingIcon={AppIcon("search")},
                shape=RoundedCornerShape(16.dp),colors=TextFieldDefaults.colors(focusedIndicatorColor=Color.Transparent,unfocusedIndicatorColor=Color.Transparent))
            LazyColumn(Modifier.fillMaxWidth().heightIn(max=400.dp),state=list,verticalArrangement=Arrangement.spacedBy(4.dp)){
                items(visible,key={it.first}){(value,label)->
                    val selected=value==spec.value
                    Row(Modifier.fillMaxWidth().selectable(selected,enabled=!state.busy,role=Role.RadioButton){state.reading?.let {model.saveReadingOptions(it.withFont(setting,value))};back()}.padding(16.dp),verticalAlignment=Alignment.CenterVertically){
                        Text(label,Modifier.weight(1f),fontSize=16.sp,color=if(selected)MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurface)
                        if(selected)AppIcon("check",size=24.dp,tint=MaterialTheme.colorScheme.primary)
                    }
                }
            }
        }
    }
}

@Composable internal fun ReadingModePanel(state:ScreenState,model:ReaderModel,back:()->Unit){
    ReadingPanelPage(tr("阅读模式","Reading mode"),back,fill=false){
        Column(Modifier.fillMaxWidth().padding(16.dp),verticalArrangement=Arrangement.spacedBy(12.dp)){
            for(focus in listOf(true,false)){
                val selected=state.focus==focus
                ReadingChoiceCard(if(focus)tr("专注模式","Focus mode")else tr("经典模式","Classic mode"),selected,!state.busy&&(!focus||state.focusAvailable)){if(!selected)model.focus(focus)}
            }
            if(!state.focus)SettingsGroup {SettingToggle(tr("统一版式","Unified layout"),state.unified,!state.busy){model.configure(state.fontSize,it,state.dark)}}
        }
    }
}

@Composable internal fun ReadingSelectionPanel(state:ScreenState,model:ReaderModel,back:()->Unit){
    ReadingPanelPage(tr("文字选择模式","Selection mode"),back,fill=false){
        Column(Modifier.fillMaxWidth().padding(16.dp),verticalArrangement=Arrangement.spacedBy(12.dp)){
            val current=if(state.translated)"paragraph"else state.selectionMode
            for((value,title) in listOf("free" to tr("自由","Free"),"word" to tr("单词","Word"),"sentence" to tr("句子","Sentence"),"paragraph" to tr("段落","Paragraph"))){
                if(value=="word"&&!state.wordSelectionAvailable)continue
                ReadingChoiceCard(title,current==value,!state.busy&&!state.translated){if(current!=value)model.selectionMode(value);back()}
            }
        }
    }
}

@Composable private fun ReadingChoiceCard(title:String,selected:Boolean,enabled:Boolean,onClick:()->Unit){
    val accent=MaterialTheme.colorScheme.primary
    Surface(Modifier.fillMaxWidth(),shape=RoundedCornerShape(12.dp),color=if(selected)accent.copy(alpha=.05f)else MaterialTheme.colorScheme.surfaceContainerHigh.copy(alpha=.45f),
        border=if(selected)BorderStroke(1.dp,accent.copy(alpha=.6f))else null){
        Row(Modifier.fillMaxWidth().heightIn(min=56.dp).selectable(selected,enabled=enabled,role=Role.RadioButton,onClick=onClick).padding(16.dp),verticalAlignment=Alignment.CenterVertically){
            Text(title,Modifier.weight(1f),fontSize=16.sp,color=if(selected)accent else MaterialTheme.colorScheme.onSurface)
            if(selected)AppIcon("check",size=24.dp,tint=accent)
        }
    }
}

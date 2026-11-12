package org.tortotech.torto

import androidx.compose.foundation.*
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp

@Composable fun TypographyPanel(state:ScreenState,model:ReaderModel,openLayout:()->Unit={}) {
    var size by remember(state.fontSize) { mutableFloatStateOf(readingFontSize(state.fontSize)) }
    LaunchedEffect(model){model.loadFonts()}
    Column(Modifier.verticalScroll(rememberScrollState()).padding(16.dp).navigationBarsPadding(),verticalArrangement=Arrangement.spacedBy(12.dp)) {
        if(state.active==null||state.focusAvailable||!state.focus)SettingsGroup {
            if(state.active==null||state.focusAvailable)SettingToggle(tr("专注模式","Focus mode"),state.focus,!state.busy){model.focus(it)}
            if(!state.focus)SettingToggle(tr("统一版式","Unified layout"),state.unified,!state.busy){model.configure(size,it,state.dark)}
        }
        TypographyFontGroups(state,model) {
            Box(Modifier.fillMaxWidth().padding(horizontal=16.dp,vertical=12.dp)){
                ReadingFontSizeSlider(size,!state.busy,{size=it}){value->
                    size=value
                    if(value!=state.fontSize)model.configure(value,state.unified,state.dark)
                }
            }
        }
        SettingsGroup {SettingsItem(tr("AI 排版","AI layout"),enabled=!state.busy,onClick=openLayout)}
        TypographyFontDownloads(state,model)

    }
}

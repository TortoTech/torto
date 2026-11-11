package org.tortotech.torto

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

@Composable internal fun InterfaceSettingsRows(state:ScreenState,open:(String)->Unit){
    SettingsItem(tr("深色模式","Dark mode"),icon="moon",value=when(state.appTheme){
        "dark"->tr("已开启","On");"light"->tr("未开启","Off");else->tr("跟随系统","Follow system")
    }){open("dark-mode")}
    SettingsItem(tr("界面语言","Interface language"),icon="translate",value=when(state.language){
        "zh-CN"->"中文";"en"->"English";else->tr("跟随系统","Follow system")
    }){open("language")}
}

@Composable fun DarkModeScreen(state:ScreenState,model:ReaderModel){
    val systemDark=isSystemInDarkTheme()
    InterfaceChoiceScreen(state.appTheme,
        tr("开启后，将跟随系统打开或关闭深色模式","When enabled, dark mode follows your system settings"),
        listOf("light" to tr("浅色","Light"),"dark" to tr("深色","Dark")),!state.busy,
        onFollow={model.interfaceSettings(if(it)"system"else if(systemDark)"dark"else"light",state.language)},
        onSelect={model.interfaceSettings(it,state.language)})
}

@Composable fun InterfaceLanguageScreen(state:ScreenState,model:ReaderModel){
    val systemLanguage=if(LocalConfiguration.current.locales[0].language=="zh")"zh-CN"else"en"
    InterfaceChoiceScreen(state.language,
        tr("开启后，将跟随系统设置界面语言","When enabled, the interface language follows your system settings"),
        listOf("zh-CN" to "中文","en" to "English"),!state.busy,
        onFollow={model.interfaceSettings(state.appTheme,if(it)"system"else systemLanguage)},
        onSelect={model.interfaceSettings(state.appTheme,it)})
}

/** Shared follow-system switch and manual choice cards; changes save immediately. */
@Composable private fun InterfaceChoiceScreen(value:String,description:String,choices:List<Pair<String,String>>,enabled:Boolean,onFollow:(Boolean)->Unit,onSelect:(String)->Unit){
    val follows=value=="system"
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),verticalArrangement=Arrangement.spacedBy(12.dp)){
        SettingsGroup {SettingsSwitch(tr("跟随系统","Follow system"),follows,enabled,description=description,change=onFollow)}
        if(!follows){
            SectionLabel(tr("手动选择","Manual selection"))
            SettingsGroup {
                choices.forEach {(key,label)->
                    val selected=value==key
                    Row(Modifier.fillMaxWidth().heightIn(min=56.dp).selectable(selected,enabled,Role.RadioButton){onSelect(key)}.padding(horizontal=16.dp,vertical=16.dp),verticalAlignment=Alignment.CenterVertically){
                        Text(label,Modifier.weight(1f),fontSize=16.sp,color=if(selected)MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurface)
                        if(selected)AppIcon("check",size=IconSizes.Small,tint=MaterialTheme.colorScheme.primary)
                    }
                }
            }
        }
    }
}

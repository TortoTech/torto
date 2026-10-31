package org.tortotech.torto

import androidx.compose.foundation.*
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import org.tortotech.torto.core.*

@Composable internal fun SettingChoice(title:String,value:String,choices:List<Pair<String,String>>,enabled:Boolean=true,choose:(String)->Unit){
    var show by remember{mutableStateOf(false)}
    ListItem(headlineContent={Text(title)},supportingContent={Text(choices.firstOrNull {it.first==value}?.second ?: value.ifEmpty {tr("未选择","Not selected")},maxLines=2,overflow=TextOverflow.Ellipsis)},trailingContent={AppIcon("next")},modifier=Modifier.clickable(enabled=enabled){show=true},colors=ListItemDefaults.colors(containerColor=MaterialTheme.colorScheme.surface))
    if(show){var query by remember{mutableStateOf("")};AlertDialog(onDismissRequest={show=false},title={Text(title)},text={Column{
        OutlinedTextField(query,{query=it},label={Text(tr("搜索","Search"))},singleLine=true,modifier=Modifier.fillMaxWidth())
        LazyColumn(Modifier.heightIn(max=420.dp)){items(choices.filter {it.second.contains(query,true)||it.first.contains(query,true)}){(key,label)->
            ListItem(headlineContent={Text(label)},leadingContent={RadioButton(value==key,{choose(key);show=false})},modifier=Modifier.clickable {choose(key);show=false},colors=ListItemDefaults.colors(containerColor=MaterialTheme.colorScheme.surface))
        }}
    }},confirmButton={TextButton(onClick={show=false}){Text(tr("关闭","Close"))}})}
}
@Composable internal fun SettingToggle(title:String,checked:Boolean,enabled:Boolean=true,change:(Boolean)->Unit){Row(Modifier.fillMaxWidth().padding(vertical=5.dp),verticalAlignment=Alignment.CenterVertically){Text(title,Modifier.weight(1f));Switch(checked,change,enabled=enabled)}}

@Composable internal fun ProviderManager(state:ScreenState,model:ReaderModel){
    var editing by remember{mutableStateOf<Provider?>(null)}
    var deleting by remember{mutableStateOf<Provider?>(null)}
    LazyColumn(contentPadding=PaddingValues(20.dp),verticalArrangement=Arrangement.spacedBy(12.dp)){
        item{Button(onClick={editing=Provider("","custom","","",emptyList(),false)},enabled=!state.busy){AppIcon("add");Spacer(Modifier.width(8.dp));Text(tr("添加提供商","Add provider"))}}
        items(state.providers,key={it.id}){provider->Card(onClick={editing=provider}){
            Row(Modifier.fillMaxWidth().padding(16.dp),verticalAlignment=Alignment.CenterVertically){Column(Modifier.weight(1f)){
                Text(provider.name,style=MaterialTheme.typography.titleMedium)
                Text("${provider.models.size} "+tr("个模型","models"),color=MaterialTheme.colorScheme.onSurfaceVariant)
            };ActionIcon("edit",tr("编辑提供商","Edit provider"),!state.busy){editing=provider};ActionIcon("delete",tr("删除提供商","Delete provider"),!state.busy&&state.providers.size>1){deleting=provider}}
        }}
    }
    editing?.let {provider->ProviderEditor(provider,state,model){editing=null}}
    deleting?.let {provider->AlertDialog(onDismissRequest={deleting=null},title={Text(tr("删除提供商？","Delete provider?"))},text={Text(provider.name+tr("及其保存的 API Key 将被删除"," and its saved API key will be removed"))},confirmButton={TextButton(onClick={model.deleteProvider(provider.id);deleting=null}){Text(tr("删除","Delete"))}},dismissButton={TextButton(onClick={deleting=null}){Text(tr("取消","Cancel"))}})}
}
@Composable private fun ProviderEditor(initial:Provider,state:ScreenState,model:ReaderModel,close:()->Unit){
    var draft by remember(initial){mutableStateOf(initial.copy())}
    var key by remember{mutableStateOf("")};var clearKey by remember{mutableStateOf(false)}
    var manual by remember{mutableStateOf("")};var query by remember{mutableStateOf("")}
    val cacheKey=draft.kind+":"+draft.baseUrl
    val all=(draft.models+state.modelChoices[cacheKey].orEmpty()).distinct().sorted()
    Dialog(onDismissRequest={if(!state.busy)close()},properties=DialogProperties(usePlatformDefaultWidth=false)){
        Surface(Modifier.fillMaxWidth().padding(12.dp).heightIn(max=780.dp),shape=MaterialTheme.shapes.large){
            Column(Modifier.padding(20.dp).imePadding()){
                Row(Modifier.fillMaxWidth(),verticalAlignment=Alignment.CenterVertically){Text(tr("AI 提供商","AI provider"),Modifier.weight(1f),style=MaterialTheme.typography.titleLarge);ActionIcon("close",tr("关闭","Close"),!state.busy,close)}
                if(state.busy)LinearProgressIndicator(Modifier.fillMaxWidth())
                Column(Modifier.weight(1f,false).verticalScroll(rememberScrollState()),verticalArrangement=Arrangement.spacedBy(12.dp)){
                    SettingChoice(tr("提供商类型","Provider type"),draft.kind,state.providerKinds.map {it.id to it.name},!state.busy){id->val kind=state.providerKinds.first {it.id==id};draft=draft.copy(kind=id,baseUrl=kind.baseUrl,name=if(draft.name.isBlank()||state.providerKinds.any {it.name==draft.name})kind.name else draft.name)}
                    OutlinedTextField(draft.name,{draft=draft.copy(name=it.take(160))},label={Text(tr("名称","Name"))},modifier=Modifier.fillMaxWidth(),singleLine=true)
                    OutlinedTextField(draft.baseUrl,{draft=draft.copy(baseUrl=it.trim().take(2048))},label={Text("Base URL")},modifier=Modifier.fillMaxWidth(),singleLine=true)
                    OutlinedTextField(key,{key=it;clearKey=false},label={Text(if(initial.keySaved)tr("API Key（已保存，留空保留）","API key (saved; leave empty to keep)")else "API Key")},visualTransformation=PasswordVisualTransformation(),modifier=Modifier.fillMaxWidth(),singleLine=true)
                    if(initial.keySaved)SettingToggle(tr("清除已保存的 API Key","Clear saved API key"),clearKey,!state.busy){clearKey=it}
                    Row(verticalAlignment=Alignment.CenterVertically){Text(tr("模型","Models"),Modifier.weight(1f),style=MaterialTheme.typography.titleMedium);TextButton(onClick={model.fetchModels(draft,key,clearKey)},enabled=!state.busy&&draft.baseUrl.isNotBlank()){Text(tr("刷新列表","Refresh list"))}}
                    state.modelFetchError?.let {Text(it,color=MaterialTheme.colorScheme.error)}
                    Row(verticalAlignment=Alignment.CenterVertically){OutlinedTextField(manual,{manual=it.take(256)},label={Text(tr("手动添加模型 ID","Add model ID"))},modifier=Modifier.weight(1f),singleLine=true);ActionIcon("add",tr("添加模型","Add model"),manual.isNotBlank()&&!state.busy){draft=draft.copy(models=(draft.models+manual.trim()).distinct());manual=""}}
                    OutlinedTextField(query,{query=it},label={Text(tr("搜索模型","Search models"))},modifier=Modifier.fillMaxWidth(),singleLine=true)
                    LazyColumn(Modifier.heightIn(min=80.dp,max=260.dp)){
                        items(all.filter {it.contains(query,true)}){id->Row(Modifier.fillMaxWidth().clickable {draft=draft.copy(models=if(id in draft.models)draft.models-id else draft.models+id)},verticalAlignment=Alignment.CenterVertically){Checkbox(id in draft.models,{selected->draft=draft.copy(models=if(selected)draft.models+id else draft.models-id)});Text(id,maxLines=2,overflow=TextOverflow.Ellipsis)}}
                    }
                }
                Row(Modifier.fillMaxWidth(),horizontalArrangement=Arrangement.End){TextButton(onClick=close,enabled=!state.busy){Text(tr("取消","Cancel"))};Button(onClick={model.saveProvider(draft,key,clearKey,close)},enabled=!state.busy&&draft.name.isNotBlank()&&draft.baseUrl.isNotBlank()){Text(tr("保存","Save"))}}
            }
        }
    }
}
@Composable internal fun AiRoleSettings(state:ScreenState,model:ReaderModel,section:String,openProviders:()->Unit={}){
    val current=state.aiOptions
    if(current==null){CircularProgressIndicator(Modifier.padding(24.dp));return}
    var draft by remember(current){mutableStateOf(current.copy())}
    val layout=section=="layout";val translation=section=="translation"
    val provider=when{layout->draft.layoutProvider;translation->draft.translationProvider;else->draft.chatProvider}
    val selectedModel=when{layout->draft.layoutModel;translation->draft.translationModel;else->draft.chatModel}
    val reasoning=when{layout->draft.layoutReasoning;translation->draft.translationReasoning;else->draft.chatReasoning}
    val selected=state.providers.firstOrNull {it.id==provider}
    val levels=remember(selected?.kind,selectedModel){runCatching{aiReasoningLevels(selected?.kind ?: "custom",selectedModel)}.getOrDefault(listOf("default","none","low","medium","high"))}
    val levelNames=mapOf("default" to tr("模型默认","Model default"),"none" to tr("不思考","None"),"minimal" to tr("最小","Minimal"),"low" to tr("低","Low"),"medium" to tr("中","Medium"),"high" to tr("高","High"),"max" to tr("最高","Maximum"))
    Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(20.dp).imePadding().navigationBarsPadding(),verticalArrangement=Arrangement.spacedBy(12.dp)){
        if(layout)SettingToggle(tr("自动 AI 排版","Automatic AI layout"),draft.layoutEnabled,!state.busy){draft=draft.copy(layoutEnabled=it)}
        SettingChoice(tr("提供商","Provider"),provider,state.providers.map {it.id to it.name},!state.busy){id->val first=state.providers.firstOrNull {it.id==id}?.models?.firstOrNull().orEmpty();draft=when{layout->draft.copy(layoutProvider=id,layoutModel=first);translation->draft.copy(translationProvider=id,translationModel=first);else->draft.copy(chatProvider=id,chatModel=first)}}
        SettingChoice(if(layout)tr("排版模型","Layout model")else if(translation)tr("翻译模型","Translation model")else tr("对话模型","Chat model"),selectedModel,selected?.models.orEmpty().map {it to it},!state.busy){id->draft=when{layout->draft.copy(layoutModel=id);translation->draft.copy(translationModel=id);else->draft.copy(chatModel=id)}}
        TextButton(onClick=openProviders){Text(tr("管理提供商和模型","Manage providers and models"))}
        Text(if(layout)tr("使用能识图、速度快的模型（gemini/deepseek/qwen的flash系列）\n思考等级设置“不思考”","Use a fast model with vision\nReasoning effort: none")else if(translation)tr("使用文字能力强、速度快模型（gemini的flash系列）\n思考等级设置“低”或“不思考”","Use a fast model with strong language abilities\nReasoning effort: low or none")else tr("使用能识图的模型","Use a model with vision"),style=MaterialTheme.typography.bodySmall,color=MaterialTheme.colorScheme.onSurfaceVariant)
        SettingChoice(tr("思考等级","Reasoning effort"),reasoning,levels.map {it to(levelNames[it] ?: it)},!state.busy){effort->draft=when{layout->draft.copy(layoutReasoning=effort);translation->draft.copy(translationReasoning=effort);else->draft.copy(chatReasoning=effort)}}
        if(translation){
            val standardTargets=listOf("system","zh-CN","en")
            val target=if(draft.targetLanguage in standardTargets)draft.targetLanguage else "custom"
            SettingChoice(tr("目标语言","Target language"),target,listOf("system" to tr("跟随系统","System language"),"zh-CN" to "简体中文","en" to "English","custom" to tr("自定义","Custom")),!state.busy){draft=draft.copy(targetLanguage=if(it=="custom")"" else it)}
            if(target=="custom")OutlinedTextField(draft.targetLanguage,{draft=draft.copy(targetLanguage=it.take(128))},label={Text(tr("自定义语言","Custom language"))},singleLine=true,modifier=Modifier.fillMaxWidth())
            SettingToggle(tr("显示原文","Show original text"),draft.showOriginal,!state.busy){draft=draft.copy(showOriginal=it)}
            SettingToggle(tr("翻译目录","Translate contents"),draft.translateToc,!state.busy){draft=draft.copy(translateToc=it)}
            SettingToggle(tr("专家翻译","Expert translation"),draft.expertTranslation,!state.busy){draft=draft.copy(expertTranslation=it)}
        }
        if(!translation&&!layout){
            Text(tr("工具调用轮数","Tool call steps")+" ${draft.chatSteps}");Slider(draft.chatSteps.toFloat(),{draft=draft.copy(chatSteps=it.toInt().toUShort())},valueRange=1f..64f,steps=62)
            Text(tr("历史记录轮数","History turns")+" ${draft.chatHistory}");Slider(draft.chatHistory.toFloat(),{draft=draft.copy(chatHistory=it.toInt().toUShort())},valueRange=1f..50f,steps=48)
        }
        if(layout||translation)state.reading?.let {reading->SettingToggle(tr("预处理下一页","Process the next page ahead"),reading.aiPrefetch,!state.busy){model.saveReadingOptions(reading.copy(aiPrefetch=it))}}
        Button(onClick={model.saveAiOptions(draft)},enabled=!state.busy&&(!translation||draft.targetLanguage.isNotBlank())){Text(tr("保存","Save"))}
    }
}

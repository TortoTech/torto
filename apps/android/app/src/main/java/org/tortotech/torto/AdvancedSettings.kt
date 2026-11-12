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
import org.tortotech.torto.core.*

@Composable internal fun SettingChoice(title:String,value:String,choices:List<Pair<String,String>>,enabled:Boolean=true,pageTitle:String=title,searchable:Boolean=false,choose:(String)->Unit){
    val pages=LocalChoicePages.current
    val owner=remember{Any()}
    val latestValue=rememberUpdatedState(value);val latestChoices=rememberUpdatedState(choices)
    val latestEnabled=rememberUpdatedState(enabled);val latestChoose=rememberUpdatedState(choose)
    DisposableEffect(pages,owner){onDispose{pages.release(owner)}}
    SettingsItem(title,value=choices.firstOrNull {it.first==value}?.second ?: value.ifEmpty {tr("未选择","Not selected")},enabled=enabled){
        pages.open(ChoiceRequest(owner,pageTitle,{latestValue.value},{latestChoices.value},{latestEnabled.value},{latestChoose.value(it)},searchable))
    }
}
@Composable internal fun SettingToggle(title:String,checked:Boolean,enabled:Boolean=true,change:(Boolean)->Unit){SettingsSwitch(title,checked,enabled,change=change)}

@Composable internal fun ProviderManager(state:ScreenState,model:ReaderModel,edit:(Provider)->Unit){
    var deleting by remember{mutableStateOf<Provider?>(null)}
    LazyColumn(contentPadding=PaddingValues(16.dp),verticalArrangement=Arrangement.spacedBy(12.dp)){
        items(state.providers,key={it.id}){provider->Card(onClick={edit(provider)},shape=androidx.compose.foundation.shape.RoundedCornerShape(16.dp),colors=CardDefaults.cardColors(containerColor=MaterialTheme.colorScheme.surfaceContainerLowest)){
            Row(Modifier.fillMaxWidth().padding(16.dp),verticalAlignment=Alignment.CenterVertically){Column(Modifier.weight(1f)){
                Text(provider.name,style=MaterialTheme.typography.titleMedium)
                Text("${provider.models.size} "+tr("个模型","models"),color=MaterialTheme.colorScheme.onSurfaceVariant)
            };ActionIcon("edit",tr("编辑提供商","Edit provider"),!state.busy){edit(provider)};ActionIcon("delete",tr("删除提供商","Delete provider"),!state.busy&&state.providers.size>1){deleting=provider}}
        }}
    }
    deleting?.let {provider->AlertDialog(onDismissRequest={deleting=null},title={Text(tr("删除提供商？","Delete provider?"))},text={Text(provider.name+tr("及其保存的 API Key 将被删除"," and its saved API key will be removed"))},confirmButton={TextButton(onClick={model.deleteProvider(provider.id);deleting=null}){Text(tr("删除","Delete"))}},dismissButton={TextButton(onClick={deleting=null}){Text(tr("取消","Cancel"))}})}
}
@Composable internal fun ProviderEditor(initial:Provider,state:ScreenState,model:ReaderModel,close:()->Unit){
    var draft by remember(initial){mutableStateOf(initial.copy())}
    var key by remember{mutableStateOf("")}
    var manual by remember{mutableStateOf("")};var query by remember{mutableStateOf("")}
    val cacheKey=draft.kind+":"+draft.baseUrl
    val all=(draft.models+state.modelChoices[cacheKey].orEmpty()).distinct().sorted()
    Column(Modifier.fillMaxSize().padding(16.dp).imePadding()){
        Column(Modifier.weight(1f).verticalScroll(rememberScrollState()),verticalArrangement=Arrangement.spacedBy(12.dp)){
            SettingsGroup {
            SettingChoice(tr("提供商类型","Provider type"),draft.kind,state.providerKinds.map {it.id to it.name},!state.busy){id->val kind=state.providerKinds.first {it.id==id};draft=draft.copy(kind=id,baseUrl=kind.baseUrl,name=if(draft.name.isBlank()||state.providerKinds.any {it.name==draft.name})kind.name else draft.name)}
            Column(Modifier.padding(horizontal=16.dp,vertical=12.dp),verticalArrangement=Arrangement.spacedBy(12.dp)){
            OutlinedTextField(draft.name,{draft=draft.copy(name=it.take(160))},label={Text(tr("名称","Name"))},modifier=Modifier.fillMaxWidth(),singleLine=true)
            OutlinedTextField(draft.baseUrl,{draft=draft.copy(baseUrl=it.trim().take(2048))},label={Text("Base URL")},modifier=Modifier.fillMaxWidth(),singleLine=true)
            OutlinedTextField(key,{key=it},label={Text(if(initial.keySaved)tr("API Key（已保存，留空保留）","API key (saved; leave empty to keep)")else "API Key")},visualTransformation=PasswordVisualTransformation(),modifier=Modifier.fillMaxWidth(),singleLine=true)
            }
            }
            SettingsGroup { Column(Modifier.padding(16.dp),verticalArrangement=Arrangement.spacedBy(12.dp)){
            Row(verticalAlignment=Alignment.CenterVertically){Text(tr("模型","Models"),Modifier.weight(1f),style=MaterialTheme.typography.titleMedium);ActionIcon("sync",tr("刷新模型列表","Refresh model list"),!state.busy&&draft.baseUrl.isNotBlank()){model.fetchModels(draft,key)}}
            state.modelFetchError?.let {Text(it,color=MaterialTheme.colorScheme.error)}
            Row(verticalAlignment=Alignment.CenterVertically){OutlinedTextField(manual,{manual=it.take(256)},label={Text(tr("手动添加模型 ID","Add model ID"))},modifier=Modifier.weight(1f),singleLine=true);ActionIcon("add",tr("添加模型","Add model"),manual.isNotBlank()&&!state.busy){draft=draft.copy(models=(draft.models+manual.trim()).distinct());manual=""}}
            OutlinedTextField(query,{query=it},label={Text(tr("搜索模型","Search models"))},modifier=Modifier.fillMaxWidth(),singleLine=true)
            LazyColumn(Modifier.heightIn(min=80.dp,max=260.dp)){
                items(all.filter {it.contains(query,true)}){id->Row(Modifier.fillMaxWidth().clickable {draft=draft.copy(models=if(id in draft.models)draft.models-id else draft.models+id)},verticalAlignment=Alignment.CenterVertically){Checkbox(id in draft.models,{selected->draft=draft.copy(models=if(selected)draft.models+id else draft.models-id)});Text(id,maxLines=2,overflow=TextOverflow.Ellipsis)}}
            }
            }}
        }
        Row(Modifier.fillMaxWidth(),horizontalArrangement=Arrangement.End){TextButton(onClick=close,enabled=!state.busy){Text(tr("取消","Cancel"))};Button(onClick={model.saveProvider(draft,key,false,close)},enabled=!state.busy&&draft.name.isNotBlank()&&draft.baseUrl.isNotBlank()){Text(tr("保存","Save"))}}
    }
}
@Composable internal fun AiRoleSettings(state:ScreenState,model:ReaderModel,section:String){
    val current=state.aiOptions
    if(current==null){CircularProgressIndicator(Modifier.padding(24.dp));return}
    val layout=section=="layout";val translation=section=="translation"
    var baseline by remember(section){mutableStateOf(current)}
    var draft by remember(section){mutableStateOf(current.copy())}
    fun changeOptions(change:(AiOptions)->AiOptions){draft=change(draft)}
    val standardTargets=listOf("system","zh-CN","en")
    var customTarget by remember(section){mutableStateOf(current.targetLanguage !in standardTargets)}
    var customLanguage by remember(section){mutableStateOf(current.targetLanguage.takeIf {it !in standardTargets}.orEmpty())}
    var targetEdited by remember(section){mutableStateOf(false)}
    LaunchedEffect(section,current){
        // Refresh untouched fields if an earlier page's save completes late.
        draft=mergeAiRoleDraft(current,baseline,draft,section)
        baseline=current
        if(!targetEdited){
            customTarget=current.targetLanguage !in standardTargets
            customLanguage=current.targetLanguage.takeIf {it !in standardTargets}.orEmpty()
        }
    }
    DisposableEffect(section,model){onDispose{
        // Header back, Android back and sheet closure all commit the current
        // role's edits. Empty custom input retains the last valid language.
        val original=baseline
        val pending=if(translation&&customTarget)draft.copy(targetLanguage=customLanguage.trim().ifEmpty {original.targetLanguage})else draft
        if(pending!=original)model.updateAiOptions {mergeAiRoleDraft(it,original,pending,section)}
    }}
    val provider=when{layout->draft.layoutProvider;translation->draft.translationProvider;else->draft.chatProvider}
    val selectedModel=when{layout->draft.layoutModel;translation->draft.translationModel;else->draft.chatModel}
    val reasoning=when{layout->draft.layoutReasoning;translation->draft.translationReasoning;else->draft.chatReasoning}
    val selected=state.providers.firstOrNull {it.id==provider}
    val levels=remember(selected?.kind,selectedModel){runCatching{aiReasoningLevels(selected?.kind ?: "custom",selectedModel)}.getOrDefault(listOf("default","none","low","medium","high"))}
    val levelNames=mapOf("default" to tr("模型默认","Model default"),"none" to tr("不思考","None"),"minimal" to tr("最小","Minimal"),"low" to tr("低","Low"),"medium" to tr("中","Medium"),"high" to tr("高","High"),"max" to tr("最高","Maximum"))
    Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(16.dp).imePadding().navigationBarsPadding(),verticalArrangement=Arrangement.spacedBy(12.dp)){
        if(layout)SettingsGroup {SettingToggle(tr("AI 排版","AI layout"),draft.layoutEnabled,!state.busy){enabled->changeOptions {it.copy(layoutEnabled=enabled)}}}
        SettingsGroup {
        SettingChoice(tr("提供商","Provider"),provider,state.providers.map {it.id to it.name},!state.busy){id->val first=state.providers.firstOrNull {it.id==id}?.models?.firstOrNull().orEmpty();changeOptions {value->when{layout->value.copy(layoutProvider=id,layoutModel=first);translation->value.copy(translationProvider=id,translationModel=first);else->value.copy(chatProvider=id,chatModel=first)}}}
        SettingChoice(if(layout)tr("排版模型","Layout model")else if(translation)tr("翻译模型","Translation model")else tr("对话模型","Chat model"),selectedModel,selected?.models.orEmpty().map {it to it},!state.busy,searchable=true){id->changeOptions {value->when{layout->value.copy(layoutModel=id);translation->value.copy(translationModel=id);else->value.copy(chatModel=id)}}}
        SettingChoice(tr("思考等级","Reasoning effort"),reasoning,levels.map {it to(levelNames[it] ?: it)},!state.busy){effort->changeOptions {value->when{layout->value.copy(layoutReasoning=effort);translation->value.copy(translationReasoning=effort);else->value.copy(chatReasoning=effort)}}}
        }
        SettingsNote(if(layout)tr("使用能识图、速度快的模型（gemini/deepseek/qwen的flash系列）\n思考等级设置“不思考”","Use a fast model with vision\nReasoning effort: none")else if(translation)tr("使用文字能力强、速度快模型（gemini的flash系列）\n思考等级设置“低”或“不思考”","Use a fast model with strong language abilities\nReasoning effort: low or none")else tr("使用能识图的模型","Use a model with vision"))
        if(translation){
            SettingsGroup {
            val target=if(customTarget)"custom"else draft.targetLanguage
            SettingChoice(tr("目标语言","Target language"),target,listOf("system" to tr("跟随系统","System language"),"zh-CN" to "简体中文","en" to "English","custom" to tr("自定义","Custom")),!state.busy){language->
                customTarget=language=="custom"
                targetEdited=true
                if(!customTarget)changeOptions {it.copy(targetLanguage=language)}
            }
            if(customTarget)OutlinedTextField(customLanguage,{customLanguage=it.take(128);targetEdited=true},label={Text(tr("自定义语言","Custom language"))},singleLine=true,modifier=Modifier.fillMaxWidth().padding(horizontal=16.dp,vertical=8.dp))
            SettingToggle(tr("显示原文","Show original text"),draft.showOriginal,!state.busy){enabled->changeOptions {it.copy(showOriginal=enabled)}}
            SettingToggle(tr("翻译目录","Translate contents"),draft.translateToc,!state.busy){enabled->changeOptions {it.copy(translateToc=enabled)}}
            SettingToggle(tr("专家翻译","Expert translation"),draft.expertTranslation,!state.busy){enabled->changeOptions {it.copy(expertTranslation=enabled)}}
            }
        }
    }
}

/** Apply only edited fields of this role to the latest persisted configuration. */
internal fun mergeAiRoleDraft(latest:AiOptions,original:AiOptions,edited:AiOptions,section:String):AiOptions=when(section){
    "layout"->latest.copy(
        layoutEnabled=if(edited.layoutEnabled!=original.layoutEnabled)edited.layoutEnabled else latest.layoutEnabled,
        layoutProvider=if(edited.layoutProvider!=original.layoutProvider)edited.layoutProvider else latest.layoutProvider,
        layoutModel=if(edited.layoutModel!=original.layoutModel)edited.layoutModel else latest.layoutModel,
        layoutReasoning=if(edited.layoutReasoning!=original.layoutReasoning)edited.layoutReasoning else latest.layoutReasoning,
    )
    "translation"->latest.copy(
        translationProvider=if(edited.translationProvider!=original.translationProvider)edited.translationProvider else latest.translationProvider,
        translationModel=if(edited.translationModel!=original.translationModel)edited.translationModel else latest.translationModel,
        translationReasoning=if(edited.translationReasoning!=original.translationReasoning)edited.translationReasoning else latest.translationReasoning,
        targetLanguage=if(edited.targetLanguage!=original.targetLanguage)edited.targetLanguage else latest.targetLanguage,
        showOriginal=if(edited.showOriginal!=original.showOriginal)edited.showOriginal else latest.showOriginal,
        translateToc=if(edited.translateToc!=original.translateToc)edited.translateToc else latest.translateToc,
        expertTranslation=if(edited.expertTranslation!=original.expertTranslation)edited.expertTranslation else latest.expertTranslation,
    )
    else->latest.copy(
        chatProvider=if(edited.chatProvider!=original.chatProvider)edited.chatProvider else latest.chatProvider,
        chatModel=if(edited.chatModel!=original.chatModel)edited.chatModel else latest.chatModel,
        chatReasoning=if(edited.chatReasoning!=original.chatReasoning)edited.chatReasoning else latest.chatReasoning,
    )
}

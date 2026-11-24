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
import androidx.compose.ui.unit.sp
import org.tortotech.torto.core.*

@Composable internal fun SettingChoice(title:String,value:String,choices:List<Pair<String,String>>,enabled:Boolean=true,pageTitle:String=title,description:String?=null,searchable:Boolean=false,choose:(String)->Unit){
    val pages=LocalChoicePages.current
    val owner=remember{Any()}
    val latestValue=rememberUpdatedState(value);val latestChoices=rememberUpdatedState(choices)
    val latestEnabled=rememberUpdatedState(enabled);val latestChoose=rememberUpdatedState(choose)
    DisposableEffect(pages,owner){onDispose{pages.release(owner)}}
    SettingsItem(title,value=choices.firstOrNull {it.first==value}?.second ?: value.ifEmpty {tr("未选择","Not selected")},description=description,enabled=enabled){
        pages.open(ChoiceRequest(owner,pageTitle,{latestValue.value},{latestChoices.value},{latestEnabled.value},{latestChoose.value(it)},searchable))
    }
}
@Composable internal fun SettingToggle(title:String,checked:Boolean,enabled:Boolean=true,change:(Boolean)->Unit){SettingsSwitch(title,checked,enabled,change=change)}

@Composable internal fun ProviderManager(state:ScreenState,model:ReaderModel,edit:(Provider)->Unit){
    var deleting by remember{mutableStateOf<Provider?>(null)}
    LazyColumn(contentPadding=PaddingValues(start=16.dp,end=16.dp,top=8.dp,bottom=16.dp),verticalArrangement=Arrangement.spacedBy(12.dp)){
        items(state.providers,key={it.id}){provider->Card(onClick={edit(provider)},shape=androidx.compose.foundation.shape.RoundedCornerShape(16.dp),colors=CardDefaults.cardColors(containerColor=MaterialTheme.colorScheme.surfaceContainerLowest)){
            Row(Modifier.fillMaxWidth().padding(16.dp),verticalAlignment=Alignment.CenterVertically){Column(Modifier.weight(1f)){
                Text(provider.name,fontSize=16.sp,lineHeight=20.sp,style=MaterialTheme.typography.titleMedium)
                Spacer(Modifier.height(2.dp))
                Text("${provider.models.size} "+tr("个模型","models"),fontSize=12.sp,lineHeight=16.sp,color=MaterialTheme.colorScheme.onSurfaceVariant)
            };ActionIcon("edit",tr("编辑提供商","Edit provider"),!state.busy){edit(provider)};ActionIcon("delete",tr("删除提供商","Delete provider"),!state.busy&&state.providers.size>1){deleting=provider}}
        }}
    }
    deleting?.let {provider->AlertDialog(onDismissRequest={deleting=null},title={Text(tr("删除提供商？","Delete provider?"))},text={Text(provider.name+tr("及其保存的 API Key 将被删除"," and its saved API key will be removed"))},confirmButton={TextButton(onClick={model.deleteProvider(provider.id);deleting=null}){Text(tr("删除","Delete"))}},dismissButton={TextButton(onClick={deleting=null}){Text(tr("取消","Cancel"))}})}
}
@Composable internal fun ProviderEditor(initial:Provider,state:ScreenState,model:ReaderModel,close:()->Unit){
    var draft by remember(initial){mutableStateOf(initial.copy())}
    var key by remember{mutableStateOf("")}
    var query by remember{mutableStateOf("")}
    val cacheKey=draft.kind+":"+draft.baseUrl
    LaunchedEffect(initial.id, cacheKey){ model.loadCachedModels(initial.id, cacheKey) }
    DisposableEffect(initial.id, model){ onDispose {
        val pending=draft.copy(name=draft.name.trim(),baseUrl=draft.baseUrl.trim())
        if((pending!=initial||key.isNotEmpty())&&pending.name.isNotBlank()&&pending.baseUrl.isNotBlank()){
            model.saveProvider(pending,key,false)
        }
    }}
    val all=(draft.models+state.modelChoices[cacheKey].orEmpty()).distinct().sortedWith(compareBy<String>({it !in draft.models},{it.lowercase()}))
    Column(Modifier.fillMaxSize().padding(start=16.dp,end=16.dp,top=8.dp,bottom=16.dp).windowInsetsPadding(WindowInsets.navigationBars.union(WindowInsets.ime))){
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
            Row(verticalAlignment=Alignment.CenterVertically){Text(tr("模型","Models"),Modifier.weight(1f),style=MaterialTheme.typography.titleMedium);ActionIcon("sync",if(state.fetchingModels)tr("正在刷新模型列表…","Refreshing model list…")else tr("刷新模型列表","Refresh model list"),enabled=draft.baseUrl.isNotBlank(),spinning=state.fetchingModels){model.fetchModels(draft,key)}}
            state.modelFetchError?.let {Text(it,color=MaterialTheme.colorScheme.error)}
            OutlinedTextField(query,{query=it},label={Text(tr("搜索模型","Search models"))},modifier=Modifier.fillMaxWidth(),singleLine=true)
            LazyColumn(Modifier.heightIn(min=80.dp,max=260.dp)){
                items(all.filter {it.contains(query,true)}){id->Row(Modifier.fillMaxWidth().clickable {draft=draft.copy(models=if(id in draft.models)draft.models-id else draft.models+id)},verticalAlignment=Alignment.CenterVertically){Checkbox(id in draft.models,{selected->draft=draft.copy(models=if(selected)draft.models+id else draft.models-id)});Text(id,maxLines=2,overflow=TextOverflow.Ellipsis)}}
            }
            }}
        }
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
    var layoutProtocol by remember(provider, selectedModel, selected?.baseUrl) { mutableStateOf("") }
    val isDecision = layoutProtocol == "decision" || (selected != null &&
        isLayoutDecisionModel(selected.baseUrl, selectedModel))
    LaunchedEffect(layout, provider, selectedModel, selected?.baseUrl) {
        if (layout && selectedModel.isNotBlank() && selected != null) {
            layoutProtocol = try {
                model.probeLayoutModel(provider, selectedModel)
            } catch (cancelled: kotlinx.coroutines.CancellationException) {
                throw cancelled
            } catch (_: Exception) {
                "unconfirmed"
            }
        }
    }
    LaunchedEffect(layout, layoutProtocol, provider, selectedModel) {
        if (layout && layoutProtocol == "chat" && draft.imageModel.isEmpty()) {
            changeOptions {
                it.copy(imageProvider = provider, imageModel = selectedModel,
                    imageReasoning = it.layoutReasoning)
            }
        }
    }
    val levels=remember(selected?.kind,selectedModel){runCatching{aiReasoningLevels(selected?.kind ?: "custom",selectedModel)}.getOrDefault(listOf("default","none","low","medium","high"))}
    val levelNames=mapOf("default" to tr("模型默认","Model default"),"none" to tr("不思考","None"),"minimal" to tr("最小","Minimal"),"low" to tr("低","Low"),"medium" to tr("中","Medium"),"high" to tr("高","High"),"max" to tr("最高","Maximum"))
    Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(start=16.dp,end=16.dp,top=8.dp,bottom=16.dp).windowInsetsPadding(WindowInsets.navigationBars.union(WindowInsets.ime)),verticalArrangement=Arrangement.spacedBy(12.dp)){
        if(layout)SettingsGroup {SettingToggle(tr("AI 排版","AI layout"),draft.layoutEnabled,!state.busy){enabled->changeOptions {it.copy(layoutEnabled=enabled)}}}
        val allModels=state.providers.flatMap {p->
            val multi=state.providers.size>1
            p.models.map {m->"${p.id}\n$m" to if(multi)"$m · ${p.name}" else m}
        }
        val selectedKey=if(selectedModel.isNotEmpty())"$provider\n$selectedModel" else ""
        val modelDesc=if(layout)tr("具备视觉能力的决策/文本模型","Vision-capable decision or text model")else if(translation)null else tr("要求视觉能力","Requires vision")
        val effortDesc=if(translation)tr("建议设为「低」或「不思考」","Recommended: Low or None")else null
        SettingsGroup {
        SettingChoice(if(layout)tr("排版分类模型","Layout classification model")else if(translation)tr("翻译模型","Translation model")else tr("对话模型","Chat model"),selectedKey,allModels,!state.busy,description=modelDesc,searchable=true){key->
            val idx=key.indexOf('\n'); if(idx>=0){
                val pId=key.substring(0,idx); val mId=key.substring(idx+1)
                changeOptions {value->when{layout->value.copy(layoutProvider=pId,layoutModel=mId);translation->value.copy(translationProvider=pId,translationModel=mId);else->value.copy(chatProvider=pId,chatModel=mId)}}
            }
        }
        if (!layout || !isDecision) {
            SettingChoice(tr("思考等级","Reasoning effort"),reasoning,levels.map {it to(levelNames[it] ?: it)},!state.busy,description=effortDesc){effort->changeOptions {value->when{layout->value.copy(layoutReasoning=effort);translation->value.copy(translationReasoning=effort);else->value.copy(chatReasoning=effort)}}}
        }
        }
        if (layout) {
            val imageProvider = state.providers.firstOrNull { it.id == draft.imageProvider }
            val multi = state.providers.size > 1
            val imageModels = state.providers.flatMap { candidate ->
                candidate.models.filterNot { isLayoutDecisionModel(candidate.baseUrl, it) }
                    .map { m -> "${candidate.id}\n$m" to if (multi) "$m · ${candidate.name}" else m }
            }
            val imageKey = if (draft.imageModel.isNotEmpty()) "${draft.imageProvider}\n${draft.imageModel}" else ""
            val imageLevels = remember(imageProvider?.kind, draft.imageModel) {
                runCatching { aiReasoningLevels(imageProvider?.kind ?: "custom", draft.imageModel) }
                    .getOrDefault(listOf("default", "none", "low", "medium", "high"))
            }
            SettingsGroup {
                SettingChoice(tr("排版生成模型", "Layout generation model"), imageKey,
                    imageModels, !state.busy,
                    description = tr("要求视觉能力", "Requires vision"),
                    searchable = true
                ) { key ->
                    val idx = key.indexOf('\n'); if (idx >= 0) {
                        changeOptions { it.copy(imageProvider = key.substring(0, idx), imageModel = key.substring(idx + 1)) }
                    }
                }
                SettingChoice(tr("生成模型思考等级", "Generation reasoning effort"),
                    draft.imageReasoning, imageLevels.map { it to (levelNames[it] ?: it) },
                    !state.busy,
                    description = tr("建议设为「不思考」", "Recommended: None")
                ) { effort -> changeOptions { it.copy(imageReasoning = effort) } }
            }
        }
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
            SettingToggle(tr("提取术语表","Extract glossary"),draft.expertTranslation,!state.busy){enabled->changeOptions {it.copy(expertTranslation=enabled)}}
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
        imageProvider=if(edited.imageProvider!=original.imageProvider)edited.imageProvider else latest.imageProvider,
        imageModel=if(edited.imageModel!=original.imageModel)edited.imageModel else latest.imageModel,
        imageReasoning=if(edited.imageReasoning!=original.imageReasoning)edited.imageReasoning else latest.imageReasoning,
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

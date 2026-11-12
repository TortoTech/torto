package org.tortotech.torto
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter

internal fun localDateTime(ms:Long):String=if(ms<=0)"—" else DateTimeFormatter.ofPattern("yyyy-MM-dd HH:mm").format(Instant.ofEpochMilli(ms).atZone(ZoneId.systemDefault()))
@Composable internal fun CloudSettingsPanel(state:ScreenState,model:ReaderModel){
    val saved=state.cloud ?: return
    var draft by remember(saved){mutableStateOf(saved.copy())};var password by remember{mutableStateOf("")}
    Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(16.dp).imePadding(),verticalArrangement=Arrangement.spacedBy(12.dp)){
        SettingsGroup {SettingToggle(tr("启用云同步","Enable cloud sync"),draft.enabled,!state.busy){draft=draft.copy(enabled=it)}}
        SettingsGroup {
        SettingChoice(tr("云盘提供商","Cloud provider"),draft.provider,state.cloudPresets.map {it.id to it.name},!state.busy){id->val preset=state.cloudPresets.first {it.id==id};draft=draft.copy(provider=id,url=preset.url)}
        Column(Modifier.padding(horizontal=16.dp,vertical=12.dp),verticalArrangement=Arrangement.spacedBy(12.dp)){
        OutlinedTextField(draft.url,{draft=draft.copy(url=it.take(2048))},label={Text(tr("WebDAV 地址","WebDAV URL"))},singleLine=true,modifier=Modifier.fillMaxWidth(),readOnly=draft.provider!="custom")
        OutlinedTextField(draft.username,{draft=draft.copy(username=it.take(512))},label={Text(tr("用户名","Username"))},singleLine=true,modifier=Modifier.fillMaxWidth())
        OutlinedTextField(password,{password=it},label={Text(if(saved.passwordSaved)tr("密码（已保存，留空保留）","Password (saved; leave empty to keep)") else tr("密码","Password"))},singleLine=true,visualTransformation=PasswordVisualTransformation(),modifier=Modifier.fillMaxWidth())
        OutlinedTextField(draft.deviceName,{draft=draft.copy(deviceName=it.take(160))},label={Text(tr("设备名称","Device name"))},singleLine=true,modifier=Modifier.fillMaxWidth())
        }
        }
        SettingsGroup { Column(Modifier.padding(16.dp),verticalArrangement=Arrangement.spacedBy(12.dp)){
        val status=state.cloudStatus
        CloudSyncSummary(status)
        if(status.running)TextButton(onClick={CloudRuntime.cancel()}){Text(tr("停止同步","Stop sync"))}
        else Text(tr("上次同步：","Last sync: ")+localDateTime(status.lastSuccess),style=MaterialTheme.typography.bodySmall)
        Row(horizontalArrangement=Arrangement.spacedBy(12.dp)){Button(onClick={model.saveCloud(draft,password,false){password=""}},enabled=!state.busy){Text(tr("保存","Save"))};OutlinedButton(onClick=model::synchronize,enabled=saved.enabled&&!status.running){Text(tr("同步","Sync"))}}
        }}
    }
}

@Composable internal fun CloudSyncSummary(status:CloudStatus,modifier:Modifier=Modifier){
    if(!status.running && status.error==null && status.retryAt==0L)return
    Surface(modifier.fillMaxWidth(),shape=MaterialTheme.shapes.medium,color=MaterialTheme.colorScheme.surfaceContainerLow){
        Column(Modifier.padding(12.dp),verticalArrangement=Arrangement.spacedBy(6.dp)){
            if(status.booksTotal>0u)Text(tr("已就绪 ${status.booksCompleted}/${status.booksTotal} 本","${status.booksCompleted}/${status.booksTotal} books ready"),style=MaterialTheme.typography.labelLarge)
            if(status.running){
                Text(cloudStage(status.stage),style=MaterialTheme.typography.bodySmall)
                if(status.booksTotal>0u)LinearProgressIndicator(progress={status.booksCompleted.toFloat()/status.booksTotal.toFloat()},modifier=Modifier.fillMaxWidth())
                else if(status.total>0u)LinearProgressIndicator(progress={status.completed.toFloat()/status.total.toFloat()},modifier=Modifier.fillMaxWidth())
                else LinearProgressIndicator(Modifier.fillMaxWidth())
            }else if(status.retryAt>0)Text(tr("同步已暂停，最早重试时间：","Sync paused; retry no earlier than: ")+localDateTime(status.retryAt),style=MaterialTheme.typography.bodySmall)
            status.error?.let {Text(it,color=MaterialTheme.colorScheme.error,style=MaterialTheme.typography.bodySmall)}
        }
    }
}
@Composable private fun cloudStage(stage:String)=when(stage){"preparing"->tr("准备同步","Preparing sync");"checking"->tr("检查云端变更","Checking cloud changes");"uploading"->tr("上传书籍","Uploading books");"downloading"->tr("下载书籍","Downloading books");"derived"->tr("同步 PDF 处理数据","Syncing PDF processing data");"statistics"->tr("同步阅读统计","Syncing reading statistics");else->tr("同步进度与批注","Syncing progress and annotations")}

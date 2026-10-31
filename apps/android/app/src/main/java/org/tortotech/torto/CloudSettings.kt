package org.tortotech.torto
import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter

internal fun localDateTime(ms:Long):String=if(ms<=0)"—" else DateTimeFormatter.ofPattern("yyyy-MM-dd HH:mm").format(Instant.ofEpochMilli(ms).atZone(ZoneId.systemDefault()))
@Composable internal fun CloudSettingsPanel(state:ScreenState,model:ReaderModel){
    val saved=state.cloud ?: return
    var draft by remember(saved){mutableStateOf(saved.copy())};var password by remember{mutableStateOf("")};var clear by remember{mutableStateOf(false)}
    val context=LocalContext.current
    Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(20.dp).imePadding(),verticalArrangement=Arrangement.spacedBy(12.dp)){
        SettingToggle(tr("启用云同步","Enable cloud sync"),draft.enabled,!state.busy){draft=draft.copy(enabled=it)}
        SettingChoice(tr("云盘提供商","Cloud provider"),draft.provider,state.cloudPresets.map {it.id to it.name},!state.busy){id->val preset=state.cloudPresets.first {it.id==id};draft=draft.copy(provider=id,url=preset.url)}
        OutlinedTextField(draft.url,{draft=draft.copy(url=it.take(2048))},label={Text(tr("WebDAV 地址","WebDAV URL"))},singleLine=true,modifier=Modifier.fillMaxWidth(),readOnly=draft.provider!="custom")
        OutlinedTextField(draft.username,{draft=draft.copy(username=it.take(512))},label={Text(tr("用户名","Username"))},singleLine=true,modifier=Modifier.fillMaxWidth())
        OutlinedTextField(password,{password=it;clear=false},label={Text(if(saved.passwordSaved)tr("密码（已保存，留空保留）","Password (saved; leave empty to keep)") else tr("密码","Password"))},singleLine=true,visualTransformation=PasswordVisualTransformation(),modifier=Modifier.fillMaxWidth())
        if(saved.passwordSaved)SettingToggle(tr("清除已保存的密码","Clear saved password"),clear,!state.busy){clear=it}
        OutlinedTextField(draft.deviceName,{draft=draft.copy(deviceName=it.take(160))},label={Text(tr("设备名称","Device name"))},singleLine=true,modifier=Modifier.fillMaxWidth())
        SettingToggle(tr("前台自动同步","Sync automatically in foreground"),draft.foreground,!state.busy){draft=draft.copy(foreground=it)}
        SettingToggle(tr("后台自动同步","Sync automatically in background"),draft.background,!state.busy){draft=draft.copy(background=it)}
        Text(tr("后台同步由系统调度，省电模式下可能延后","Background sync is scheduled by Android and may be delayed by battery optimization"),style=MaterialTheme.typography.bodySmall,color=MaterialTheme.colorScheme.onSurfaceVariant)
        val status=state.cloudStatus
        if(status.running){Text(cloudStage(status.stage));if(status.total>0u)LinearProgressIndicator(progress={status.completed.toFloat()/status.total.toFloat()},modifier=Modifier.fillMaxWidth())else LinearProgressIndicator(Modifier.fillMaxWidth());TextButton(onClick={CloudRuntime.cancel()}){Text(tr("停止同步","Stop sync"))}}
        else Text(tr("上次同步：","Last sync: ")+localDateTime(status.lastSuccess),style=MaterialTheme.typography.bodySmall)
        status.error?.let {Text(it,color=MaterialTheme.colorScheme.error)}
        Row(horizontalArrangement=Arrangement.spacedBy(12.dp)){Button(onClick={model.saveCloud(draft,password,clear){password="";clear=false}},enabled=!state.busy){Text(tr("保存","Save"))};OutlinedButton(onClick=model::synchronize,enabled=saved.enabled&&!status.running){Text(tr("立即同步","Sync now"))}}
        state.cloudPresets.firstOrNull {it.id==draft.provider}?.let {preset->TextButton(onClick={context.startActivity(Intent(Intent.ACTION_VIEW,Uri.parse(preset.helpUrl)))}){Text(tr("查看云盘配置说明","Cloud setup guide"))}}
    }
}
@Composable private fun cloudStage(stage:String)=when(stage){"preparing"->tr("准备同步","Preparing sync");"checking"->tr("检查云端变更","Checking cloud changes");"uploading"->tr("上传书籍","Uploading books");"downloading"->tr("下载书籍","Downloading books");"statistics"->tr("同步阅读统计","Syncing reading statistics");else->tr("同步进度与批注","Syncing progress and annotations")}

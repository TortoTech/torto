package org.tortotech.torto
import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.Image
import androidx.compose.ui.res.painterResource
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import org.tortotech.torto.core.updateReleaseNotes

@Composable fun AboutScreen(state:ScreenState,model:ReaderModel,licenses:()->Unit={}){
    val context=LocalContext.current
    val cache=remember{AssistantAssetCache()}
    val language=tr("zh-CN","en")
    val notes=remember(state.appUpdate?.notes,language){updateReleaseNotes(state.appUpdate?.notes.orEmpty(),language)}
    Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(24.dp).navigationBarsPadding(),verticalArrangement=Arrangement.spacedBy(16.dp)){
        Image(painterResource(R.drawable.ic_torto),tr("Torto 徽标","Torto logo"),Modifier.size(64.dp));Text("Torto",style=MaterialTheme.typography.headlineLarge);Text(state.appVersion)
        Text(tr("专注阅读的开源电子书阅读器","An open-source ebook reader focused on reading"))
        SettingChoice(tr("更新来源","Update source"),state.updateSource,listOf("automatic" to tr("自动","Automatic"),"github" to "GitHub","gitee" to "Gitee"),!state.checkingUpdate&&!state.downloadingUpdate,model::updateSource)
        Button(onClick=model::checkUpdates,enabled=!state.checkingUpdate&&!state.downloadingUpdate){Text(tr("检查更新","Check for updates"))}
        if(state.checkingUpdate)LinearProgressIndicator(Modifier.fillMaxWidth())
        if(state.updateChecked&&state.appUpdate==null)Text(tr("当前已是最新版本","You are up to date"))
        state.appUpdate?.let {update->
            Text(tr("新版本 ","New version ")+update.version,style=MaterialTheme.typography.titleMedium)
            Text("${update.source} · ${"%.1f".format(update.size.toDouble()/1048576)} MiB",style=MaterialTheme.typography.bodySmall)
            if(notes.isNotBlank())AssistantRichText(notes,cache){url->if(url.startsWith("https://"))context.startActivity(Intent(Intent.ACTION_VIEW,Uri.parse(url)))}
            when{
                state.downloadingUpdate->{LinearProgressIndicator(progress={state.updateProgress.toFloat()/100f},modifier=Modifier.fillMaxWidth());Text("${state.updateProgress}%");TextButton(onClick=model::cancelUpdate){Text(tr("停止下载","Stop download"))}}
                state.downloadedUpdate!=null->{Text(tr("下载完成，安装前会核对应用签名","Download complete; the app signature will be checked before installation"));Button(onClick={model.prepareInstall(context)}){Text(tr("安装更新","Install update"))}}
                else->Button(onClick=model::downloadUpdate){Text(tr("下载更新","Download update"))}
            }
        }
        state.updateError?.let {Text(it,color=MaterialTheme.colorScheme.error)}
        TextButton(onClick=licenses){Text(tr("开源许可","Open-source licenses"))}
        HorizontalDivider()
        Text(tr("诊断与反馈","Diagnostics and feedback"),style=MaterialTheme.typography.titleMedium)
        OutlinedButton(onClick={model.exportDiagnostics(context)},enabled=!state.busy){Text(tr("导出诊断日志","Export diagnostic log"))}
        for((label,url)in listOf(tr("官方网站","Website") to "https://tortotech.github.io/",tr("开源项目","Source code") to "https://github.com/TortoTech/torto",tr("反馈问题","Report an issue") to "https://github.com/TortoTech/torto/issues",tr("应用许可 · AGPL-3.0-only","App license · AGPL-3.0-only") to "https://github.com/TortoTech/torto/blob/main/LICENSE"))TextButton(onClick={context.startActivity(Intent(Intent.ACTION_VIEW,Uri.parse(url)))}){Text(label)}
    }
}

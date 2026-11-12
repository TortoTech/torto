package org.tortotech.torto

import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import org.tortotech.torto.core.updateReleaseNotes

@Composable fun AboutScreen(state:ScreenState,model:ReaderModel){
    val context=LocalContext.current
    val cache=remember{AssistantAssetCache()}
    val language=tr("zh-CN","en")
    val notes=remember(state.appUpdate?.notes,language){updateReleaseNotes(state.appUpdate?.notes.orEmpty(),language)}
    LaunchedEffect(state.loaded){if(state.loaded)model.checkUpdatesIfNeeded()}
    Surface(Modifier.fillMaxSize(),color=MaterialTheme.colorScheme.surfaceContainerLowest){
        LazyColumn(Modifier.fillMaxSize(),contentPadding=PaddingValues(horizontal=24.dp,vertical=32.dp),verticalArrangement=Arrangement.SpaceBetween,horizontalAlignment=Alignment.CenterHorizontally){
            item {
                Column(Modifier.fillMaxWidth(),horizontalAlignment=Alignment.CenterHorizontally){
                    Image(painterResource(R.drawable.ic_torto),tr("Torto 徽标","Torto logo"),Modifier.padding(top=8.dp).size(104.dp))
                    Spacer(Modifier.height(20.dp))
                    Text("Torto",fontSize=28.sp,fontWeight=FontWeight.SemiBold)
                    Spacer(Modifier.height(12.dp))
                    Text(state.appVersion,fontSize=14.sp,color=MaterialTheme.colorScheme.onSurfaceVariant)
                    Spacer(Modifier.height(12.dp))
                    Text(when {
                        state.checkingUpdate||!state.updateChecked&&state.updateError==null->tr("正在检查更新","Checking for updates")
                        state.appUpdate!=null->tr("发现新版本 ","New version available: ")+state.appUpdate.version
                        state.updateError!=null->tr("暂时无法检查更新","Unable to check for updates")
                        else->tr("当前已是最新版本","You are up to date")
                    },fontSize=13.sp,color=MaterialTheme.colorScheme.onSurfaceVariant,textAlign=TextAlign.Center)
                    if(state.updateError!=null&&state.appUpdate==null)TextButton(onClick=model::checkUpdates){Text(tr("重试","Retry"))}
                    state.appUpdate?.let {
                        Spacer(Modifier.height(24.dp))
                        Column(Modifier.fillMaxWidth(),verticalArrangement=Arrangement.spacedBy(12.dp)){
                            if(notes.isNotBlank())AssistantRichText(notes,cache){url->if(url.startsWith("https://"))context.startActivity(Intent(Intent.ACTION_VIEW,Uri.parse(url)))}
                            when {
                                state.downloadingUpdate->{
                                    LinearProgressIndicator(progress={state.updateProgress.toFloat()/100f},modifier=Modifier.fillMaxWidth())
                                    Text("${state.updateProgress}%",Modifier.fillMaxWidth(),textAlign=TextAlign.Center)
                                    TextButton(onClick=model::cancelUpdate,modifier=Modifier.align(Alignment.CenterHorizontally)){Text(tr("停止下载","Stop download"))}
                                }
                                state.downloadedUpdate!=null->Button(onClick={model.prepareInstall(context)},modifier=Modifier.align(Alignment.CenterHorizontally)){Text(tr("安装更新","Install update"))}
                                else->Button(onClick=model::downloadUpdate,modifier=Modifier.align(Alignment.CenterHorizontally)){Text(tr("下载更新","Download update"))}
                            }
                            state.updateError?.let {Text(it,color=MaterialTheme.colorScheme.error)}
                        }
                    }
                }
            }
            item {
                Column(Modifier.fillMaxWidth().padding(top=32.dp),horizontalAlignment=Alignment.CenterHorizontally){
                    Text("© ${java.time.Year.now().value} TortoTech",fontSize=12.sp,color=MaterialTheme.colorScheme.onSurfaceVariant)
                    TextButton(onClick={context.startActivity(Intent(Intent.ACTION_VIEW,Uri.parse("https://tortotech.github.io/")))}){Text(tr("官方网站","Website"),fontSize=12.sp,color=MaterialTheme.colorScheme.onSurfaceVariant)}
                }
            }
        }
    }
}

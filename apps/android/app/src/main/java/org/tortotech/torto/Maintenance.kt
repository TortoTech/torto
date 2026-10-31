package org.tortotech.torto

import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import androidx.core.content.FileProvider
import java.io.File
import kotlinx.coroutines.*

internal object DiagnosticLog {
    private val scope=CoroutineScope(SupervisorJob()+Dispatchers.IO)
    private val lock=Any()
    fun record(context:Context,event:String){scope.launch {write(context,event)}}
    private fun write(context:Context,event:String){synchronized(lock){runCatching {
        val dir=File(context.filesDir,"core/logs");dir.mkdirs();val file=File(dir,"android.log")
        if(file.length()>256*1024)file.writeText("")
        file.appendText("${System.currentTimeMillis()} ${event.take(160).replace('\n',' ')}\n")
    }}}
    fun install(context:Context){val previous=Thread.getDefaultUncaughtExceptionHandler();Thread.setDefaultUncaughtExceptionHandler {thread,error->write(context,"uncaught ${error.javaClass.simpleName} thread=${thread.name}");previous?.uncaughtException(thread,error)}}
}
internal fun deviceDiagnostics(context:Context):String {
    val p=context.packageManager.getPackageInfo(context.packageName,0)
    return "App: ${p.versionName}\nPackage: ${context.packageName}\nAndroid: ${Build.VERSION.RELEASE} (API ${Build.VERSION.SDK_INT})\nDevice: ${Build.MANUFACTURER} ${Build.MODEL}\nABI: ${Build.SUPPORTED_ABIS.joinToString()}"
}
internal fun shareDiagnostics(context:Context,path:String){
    val uri=FileProvider.getUriForFile(context,context.packageName+".files",File(path))
    val intent=Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_STREAM,uri).addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
    context.startActivity(Intent.createChooser(intent,"Torto diagnostics"))
}
@Suppress("DEPRECATION")
internal fun verifyDownloadedApk(context:Context,path:String){
    val file=File(path).canonicalFile;val root=File(context.filesDir,"core/cache/updates").canonicalFile
    require(file.path.startsWith(root.path+File.separator)&&file.isFile&&file.length() in 1..256L*1024*1024){"安装包路径或大小无效"}
    val flags=if(Build.VERSION.SDK_INT>=28)PackageManager.GET_SIGNING_CERTIFICATES else PackageManager.GET_SIGNATURES
    val archive=context.packageManager.getPackageArchiveInfo(file.path,flags) ?: error("下载文件不是有效的 Android 安装包")
    require(archive.packageName==context.packageName){"安装包不属于当前应用；预览版与正式版独立安装"}
    val installed=context.packageManager.getPackageInfo(context.packageName,flags)
    val oldCode=if(Build.VERSION.SDK_INT>=28)installed.longVersionCode else installed.versionCode.toLong()
    val newCode=if(Build.VERSION.SDK_INT>=28)archive.longVersionCode else archive.versionCode.toLong()
    require(newCode>oldCode){"安装包版本没有更新"}
    val expected=if(Build.VERSION.SDK_INT>=28)installed.signingInfo?.apkContentsSigners else installed.signatures
    val actual=if(Build.VERSION.SDK_INT>=28)archive.signingInfo?.apkContentsSigners else archive.signatures
    require(!expected.isNullOrEmpty()&&!actual.isNullOrEmpty()&&expected.map {it.toCharsString()}.toSet()==actual.map {it.toCharsString()}.toSet()){"安装包签名与当前应用不一致"}
}
internal fun installDownloadedApk(context:Context,path:String){
    if(!context.packageManager.canRequestPackageInstalls()){
        context.startActivity(Intent(android.provider.Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES,Uri.parse("package:${context.packageName}")))
        return
    }
    val uri=FileProvider.getUriForFile(context,context.packageName+".files",File(path))
    context.startActivity(Intent(Intent.ACTION_VIEW).setDataAndType(uri,"application/vnd.android.package-archive").addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION))
}

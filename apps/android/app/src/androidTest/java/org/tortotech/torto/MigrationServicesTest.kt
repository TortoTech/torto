package org.tortotech.torto
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.work.WorkManager
import java.io.File
import java.util.concurrent.TimeUnit
import java.util.zip.ZipEntry
import java.util.zip.ZipInputStream
import java.util.zip.ZipOutputStream
import kotlinx.coroutines.runBlocking
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.tortotech.torto.core.*

@RunWith(AndroidJUnit4::class)
class MigrationServicesTest {
    @Test(timeout=20000) fun systemTlsRejectsAnUntrustedLoopbackCertificate():Unit=runBlocking {
        val instrumentation=InstrumentationRegistry.getInstrumentation();val context=instrumentation.targetContext
        Class.forName("org.rustls.platformverifier.CertificateVerifier")
        NativePlatform.initializeTls(context)
        val certificate=instrumentation.context.assets.open("tls-loopback-cert.der").use {java.security.cert.CertificateFactory.getInstance("X.509").generateCertificate(it)}
        val key=instrumentation.context.assets.open("tls-loopback-key.pk8").use {java.security.KeyFactory.getInstance("RSA").generatePrivate(java.security.spec.PKCS8EncodedKeySpec(it.readBytes()))}
        val store=java.security.KeyStore.getInstance(java.security.KeyStore.getDefaultType());store.load(null,null);store.setKeyEntry("loopback",key,"test-only".toCharArray(),arrayOf(certificate))
        val keys=javax.net.ssl.KeyManagerFactory.getInstance(javax.net.ssl.KeyManagerFactory.getDefaultAlgorithm());keys.init(store,"test-only".toCharArray())
        val ssl=javax.net.ssl.SSLContext.getInstance("TLS");ssl.init(keys.keyManagers,null,java.security.SecureRandom())
        val server=ssl.serverSocketFactory.createServerSocket(0,1,java.net.InetAddress.getByName("127.0.0.1")) as javax.net.ssl.SSLServerSocket;server.soTimeout=5000
        val accepted=java.util.concurrent.atomic.AtomicBoolean()
        val thread=kotlin.concurrent.thread {
            try{(server.accept() as javax.net.ssl.SSLSocket).use {socket->
                accepted.set(true);socket.soTimeout=5000;socket.startHandshake()
                val reader=socket.inputStream.bufferedReader()
                while(true){val line=reader.readLine() ?: break;if(line.isEmpty())break}
                val body="""{"data":[{"id":"must-not-trust"}]}"""
                socket.outputStream.write(("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: ${body.toByteArray().size}\r\nConnection: close\r\n\r\n"+body).toByteArray());socket.outputStream.flush()
            }}
            catch(_:Exception){}finally{server.close()}
        }
        try{
            val provider=Provider("","custom","Local TLS fixture","https://127.0.0.1:${server.localPort}/v1",emptyList(),false)
            try{NativeRuntime.core(context).fetchModels(provider,null);fail("The system trust store must reject a self-signed server")}
            catch(e:CoreException){val message=e.message.orEmpty();assertFalse(message.contains("initialized"));assertFalse(message.contains("panicked"));assertTrue("The TLS request must actually reach the self-signed server",accepted.get())}
        }finally{server.close();thread.join(5000)}
    }
    @Test(timeout=20000) fun cloudPresetsAndUniqueBackgroundScheduling():Unit=runBlocking {
        val context=InstrumentationRegistry.getInstrumentation().targetContext
        val core=NativeRuntime.core(context);val original=core.cloudOptions()
        CloudRuntime.foreground(context,false);CloudRuntime.cancel()
        try {
            assertTrue(core.cloudPresets().any {it.id=="jianguoyun"&&it.url.startsWith("https://")})
            val temporary=original.copy(enabled=true,provider="custom",url="http://127.0.0.1:1/dav",username="scheduler-test",deviceName="Android scheduler test",foreground=false,background=true)
            core.saveCloudOptions(temporary,null)
            assertEquals("Android scheduler test",core.cloudOptions().deviceName)
            val manager=WorkManager.getInstance(context)
            CloudRuntime.schedule(context)
            val first=manager.getWorkInfosForUniqueWork(CloudRuntime.PERIODIC).get(5,TimeUnit.SECONDS).first { !it.state.isFinished }
            CloudRuntime.schedule(context)
            val second=manager.getWorkInfosForUniqueWork(CloudRuntime.PERIODIC).get(5,TimeUnit.SECONDS).first { !it.state.isFinished }
            assertEquals("Repeated settings loads must keep one periodic task",first.id,second.id)
            core.saveCloudOptions(temporary.copy(enabled=false),null)
            CloudRuntime.schedule(context)
            manager.getWorkInfosForUniqueWork(CloudRuntime.PERIODIC).get(5,TimeUnit.SECONDS).forEach {assertTrue(it.state.isFinished)}
            val work=androidx.work.OneTimeWorkRequestBuilder<CloudWorker>().build()
            manager.enqueueUniqueWork("torto-disabled-worker-test",androidx.work.ExistingWorkPolicy.REPLACE,work).result.get(5,TimeUnit.SECONDS)
            val deadline=android.os.SystemClock.uptimeMillis()+10_000
            var result=manager.getWorkInfoById(work.id).get(5,TimeUnit.SECONDS)
            while(result?.state?.isFinished!=true&&android.os.SystemClock.uptimeMillis()<deadline){android.os.SystemClock.sleep(50);result=manager.getWorkInfoById(work.id).get(5,TimeUnit.SECONDS)}
            assertEquals("Disabled background workers must finish without network requests",androidx.work.WorkInfo.State.SUCCEEDED,result?.state)
            manager.cancelUniqueWork("torto-disabled-worker-test").result.get(5,TimeUnit.SECONDS)

        }finally{core.saveCloudOptions(original,null);CloudRuntime.schedule(context)}
    }
    @Test(timeout=20000) fun diagnosticSharingLicensesAndInvalidInstallers():Unit=runBlocking {
        val context=InstrumentationRegistry.getInstrumentation().targetContext
        val core=NativeRuntime.core(context)
        val secret="device-diagnostic-test-secret-7347"
        var provider:String?=null
        val log=File(context.filesDir,"core/logs/android.log");val original=if(log.exists())log.readBytes() else null
        try {
            core.saveProvider(Provider("","custom","Diagnostic test","http://127.0.0.1:1/v1",emptyList(),false),secret,false)
            provider=core.providers().single {it.name=="Diagnostic test"}.id
            log.parentFile!!.mkdirs();log.writeText("Authorization: Bearer hidden-secret\n$secret\nhealthy diagnostic\n")
            val exported=File(core.exportDiagnostics(deviceDiagnostics(context)))
            val report=exported.readText();assertTrue(report.contains("healthy diagnostic"));assertFalse(report.contains(secret));assertFalse(report.contains("hidden-secret"))
            val uri=androidx.core.content.FileProvider.getUriForFile(context,context.packageName+".files",exported)
            assertTrue(context.contentResolver.openInputStream(uri)!!.bufferedReader().use {it.readText().startsWith("Torto diagnostics")})
            try{androidx.core.content.FileProvider.getUriForFile(context,context.packageName+".files",File(context.filesDir,"core/config/private.json"));fail("Config files must not be shareable")}catch(_:IllegalArgumentException){}
            val invalid=File(context.filesDir,"core/cache/updates/test/invalid.apk");invalid.parentFile!!.mkdirs();invalid.writeText("not an APK")
            try{verifyDownloadedApk(context,invalid.path);fail("A corrupt APK must not reach the system installer")}catch(_:IllegalArgumentException){}catch(_:IllegalStateException){}finally{invalid.delete()}
            val licenses=org.json.JSONObject(context.assets.open("open-source.json").bufferedReader().use {it.readText()})
            assertTrue(licenses.getJSONArray("packages").length()>100)
            assertTrue(licenses.getJSONObject("texts").length()>0)
        }finally {provider?.let {core.deleteProvider(it)};if(original==null)log.delete()else log.writeBytes(original)}
    }
    @Test(timeout=25000) fun richFootnotesAndCalendarStatistics():Unit=runBlocking {
        val instrumentation=InstrumentationRegistry.getInstrumentation();val context=instrumentation.targetContext
        val file=File(context.cacheDir,"rich-footnote.epub")
        ZipOutputStream(file.outputStream()).use {out->ZipInputStream(instrumentation.context.assets.open("reader-interactions.epub")).use {input->
            while(true){val entry=input.nextEntry ?: break;var bytes=input.readBytes()
                if(entry.name=="package.opf")bytes=String(bytes).replace(Regex("<dc:title>.*?</dc:title>"),"<dc:title>富文本脚注测试</dc:title>").toByteArray()
                if(entry.name=="body.xhtml")bytes=String(bytes).replace("A multi-paragraph note.","<strong>A multi-paragraph note.</strong> <span class=\"math\">\\frac{1}{2}</span>").toByteArray()
                out.putNextEntry(ZipEntry(entry.name).apply {time=0L});out.write(bytes);out.closeEntry()
            }
        }}
        val core=NativeRuntime.core(context);val book=core.importBook(file.path).first {it.title=="富文本脚注测试"};val reader=core.openBook(book.id,Viewport(360u,640u,2f))
        try {
            var note:TouchAction?=null
            reader.page().use {page->
                val label=page.info().regions.first {it.text.contains("Jump internally")}.bounds.last()
                search@ for(dy in -12..12 step 4)for(dx in -8..28 step 4){
                    val action=reader.activate(label.x+label.width+dx,label.y+label.height/2+dy)
                    action.media?.close()
                    if(action.kind=="note"){note=action;break@search}
                }
            }
            assertNotNull("Authored note icon must open a popup",note)
            assertTrue(note!!.markup.contains("**A multi-paragraph note.**"));assertTrue(note!!.markup.contains("\\frac{1}{2}"))
            val rich=decodeAssistantMarkdown(chatMarkdown(note!!.markup))
            assertTrue("The authored note number must not turn into a misaligned list prefix",rich.first().prefix.isEmpty())
            assertTrue(rich.flatMap {it.runs}.any {it.math});assertTrue(rich.flatMap {it.runs}.any {it.style and 1!=0})
            reader.markFinished()
            val detail=core.bookStatistics(book.id);assertEquals("finished",detail.status);assertNotNull(detail.finished)
            assertEquals(12,core.statisticsPeriod("year",0).distribution.size)
            assertTrue(core.statisticsPeriod("month",-1).distribution.size in 28..31)
            assertTrue(core.statisticsPeriod("all",0).finished>=1u)
        }finally{reader.shutdown();reader.close();file.delete()}
    }
}

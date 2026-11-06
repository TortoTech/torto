package org.tortotech.torto

import android.view.accessibility.AccessibilityNodeInfo
import androidx.lifecycle.ViewModelProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import java.net.ServerSocket
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import java.util.zip.ZipEntry
import java.util.zip.ZipOutputStream
import kotlinx.coroutines.runBlocking
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.tortotech.torto.core.Provider

@RunWith(AndroidJUnit4::class)
class TranslationPositionTest {
    @Test(timeout=45000) fun delayedTranslationKeepsTheLatestFocusedParagraphInPlace():Unit=runBlocking {
        val instrumentation=InstrumentationRegistry.getInstrumentation()
        val context=instrumentation.targetContext
        val core=NativeRuntime.core(context)
        val originalAi=core.aiOptions();val originalReading=core.readingOptions()
        val prefs=context.getSharedPreferences("reading",0);val originalFocus=prefs.getBoolean("focus",false)
        val existing=core.books().map {it.id}.toSet()
        val entered=CountDownLatch(1);val release=CountDownLatch(1)
        val stopped=AtomicBoolean();val failure=AtomicReference<Throwable?>()
        val server=ServerSocket(0,4,java.net.InetAddress.getByName("127.0.0.1"))
        val worker=kotlin.concurrent.thread {
            try {while(!stopped.get())server.accept().use {socket->
                socket.soTimeout=5000
                val input=socket.getInputStream();val headers=StringBuilder()
                while(!headers.endsWith("\r\n\r\n")){val b=input.read();check(b>=0);headers.append(b.toChar())}
                val length=headers.lines().first {it.startsWith("content-length:",true)}.substringAfter(':').trim().toInt()
                val bytes=ByteArray(length);var read=0
                while(read<length){val n=input.read(bytes,read,length-read);check(n>0);read+=n}
                val content=JSONObject(bytes.toString(Charsets.UTF_8)).getJSONArray("messages").getJSONObject(1).get("content")
                val text=if(content is String)content else (content as JSONArray).let {parts->
                    (0 until parts.length()).mapNotNull {parts.getJSONObject(it).optString("text").takeIf(String::isNotEmpty)}.joinToString("\n")}
                val request=JSONObject(text.substringBefore("\n\n"))
                entered.countDown();check(release.await(10,TimeUnit.SECONDS))
                val translations=JSONObject()
                request.keys().forEach {key->if(key.firstOrNull()?.isDigit()==true)translations.put(key,"这是一段简短译文。")}
                val body=JSONObject().put("choices",JSONArray().put(JSONObject().put("index",0).put("finish_reason","stop")
                    .put("message",JSONObject().put("role","assistant").put("content",translations.toString())))).toString().toByteArray()
                socket.getOutputStream().apply {write("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: ${body.size}\r\nConnection: close\r\n\r\n".toByteArray());write(body);flush()}
            }}catch(e:Throwable){if(!stopped.get())failure.set(e)}
        }
        val file=File(context.cacheDir,"translation-position.epub")
        val paragraphs=(0..35).joinToString(""){i->"<p>Paragraph $i: "+"Stable source positions should survive a translated paragraph. ".repeat(2)+"</p>"}
        val files=mapOf(
            "mimetype" to "application/epub+zip",
            "META-INF/container.xml" to """<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="package.opf"/></rootfiles></container>""",
            "package.opf" to """<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="uid"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="uid">translation-position</dc:identifier><dc:title>Translation position test</dc:title><dc:language>en</dc:language></metadata><manifest><item id="body" href="body.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="body"/></spine></package>""",
            "body.xhtml" to """<html xmlns="http://www.w3.org/1999/xhtml"><body>$paragraphs</body></html>"""
        )
        ZipOutputStream(file.outputStream()).use {zip->files.forEach {(name,text)->zip.putNextEntry(ZipEntry(name));zip.write(text.toByteArray());zip.closeEntry()}}
        val book=core.importBook(file.absolutePath).single {it.title=="Translation position test"}
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        var activity:MainActivity?=null;var model:ReaderModel?=null;var providerId:String?=null
        fun await(message:String,condition:()->Boolean){val until=android.os.SystemClock.uptimeMillis()+10000
            while(!condition()){failure.get()?.let {throw AssertionError("Loopback model failed",it)};assertTrue("$message: ${model?.state?.value?.aiError}",android.os.SystemClock.uptimeMillis()<until);android.os.SystemClock.sleep(25)}}
        fun source():String {
            val info=model!!.state.value.frame!!.info
            val focus=info.focusBounds.first()
            val region=info.regions.first {r->r.bounds.any {b->
                val x=b.x+b.width/2;val y=b.y+b.height/2
                x>=focus.x&&x<=focus.x+focus.width&&y>=focus.y&&y<=focus.y+focus.height}}
            return JSONObject(region.source).getJSONObject("start").getString("node")
        }
        fun hasProgress(node:AccessibilityNodeInfo?):Boolean=node!=null&&(node.className=="android.widget.ProgressBar"||(0 until node.childCount).any {hasProgress(node.getChild(it))})
        try {
            check(prefs.edit().putBoolean("focus",true).commit())
            core.saveProvider(Provider("","custom","Translation position loopback","http://127.0.0.1:${server.localPort}/v1",listOf("test-model"),false),"inert-test-key",false)
            val provider=core.providers().single {it.name=="Translation position loopback"};providerId=provider.id
            core.saveReadingOptions(originalReading.copy(translated=false))
            core.saveAiOptions(originalAi.copy(layoutEnabled=false,translationProvider=provider.id,translationModel="test-model",targetLanguage="简体中文",showOriginal=false,translateToc=false,expertTranslation=false))
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity;assertNotNull(activity)
            instrumentation.runOnMainSync {model=ViewModelProvider(activity!!)[ReaderModel::class.java];model!!.open(book)}
            await("reader opens"){model!!.state.value.frame!=null&&!model!!.state.value.busy}
            instrumentation.runOnMainSync {model!!.search("Paragraph 12:")}
            await("source search completes"){model!!.state.value.hits.isNotEmpty()&&!model!!.state.value.busy}
            val hit=model!!.state.value.hits.first()
            val destination=JSONObject(hit.source).getJSONObject("start").getString("node")
            instrumentation.runOnMainSync {model!!.jump(hit)}
            await("initial focus settles"){model!!.state.value.frame!!.info.controls.any {it.kind=="sentences"}
                &&source()==destination&&!model!!.state.value.busy&&!model!!.state.value.readerMoving}
            val initial=source()
            instrumentation.runOnMainSync {model!!.translate(true)}
            assertTrue("translation waits on the real loopback transport",entered.await(10,TimeUnit.SECONDS))
            assertFalse("Translation never adds a header progress bar",hasProgress(instrumentation.uiAutomation.rootInActiveWindow))
            instrumentation.runOnMainSync {model!!.turn(true)}
            await("reading advances while the model waits"){source()!=initial&&!model!!.state.value.busy&&!model!!.state.value.readerMoving}
            val latest=source();val top=model!!.state.value.frame!!.info.focusBounds.first().y
            release.countDown()
            await("translated content reaches the current reader"){model!!.state.value.frame!!.info.regions.any {it.text.contains("简短译文")}
                &&!model!!.state.value.processing&&!model!!.state.value.busy&&!model!!.state.value.readerMoving}
            assertEquals("Completion must not return to the request's original focus",latest,source())
            assertEquals("Shortening translated paragraphs must not move the current block up",top,model!!.state.value.frame!!.info.focusBounds.first().y,8f)
            File(context.cacheDir,"translation-keeps-position.png").outputStream().use {model!!.state.value.frame!!.bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG,100,it)}
        }finally {
            release.countDown();stopped.set(true);server.close();worker.join(5000)
            instrumentation.runOnMainSync {model?.back();activity?.finish()};instrumentation.removeMonitor(monitor)
            core.saveAiOptions(originalAi);core.saveReadingOptions(originalReading)
            check(prefs.edit().putBoolean("focus",originalFocus).commit())
            providerId?.let {core.deleteProvider(it)}
            if(book.id !in existing)core.removeBook(book.id)
            file.delete()
        }
    }
}

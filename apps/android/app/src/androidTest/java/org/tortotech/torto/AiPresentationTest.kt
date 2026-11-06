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
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import java.util.zip.ZipEntry
import java.util.zip.ZipOutputStream
import kotlinx.coroutines.runBlocking
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.tortotech.torto.core.*

@RunWith(AndroidJUnit4::class)
class AiPresentationTest {
    @Test(timeout=30000) fun optionalMetadataPublishesWithoutBlockingOrHeaderProgress():Unit=runBlocking {
        val instrumentation=InstrumentationRegistry.getInstrumentation()
        val context=instrumentation.targetContext
        val core=NativeRuntime.core(context)
        val originalAi=core.aiOptions();val originalReading=core.readingOptions()
        val preferences=context.getSharedPreferences("reading",0)
        val originalFocus=preferences.getBoolean("focus",false)
        val existing=core.books().map {it.id}.toSet()
        val server=ServerSocket(0,4,java.net.InetAddress.getByName("127.0.0.1"))
        val entered=CountDownLatch(1);val release=CountDownLatch(1)
        val stopped=AtomicBoolean();val failure=AtomicReference<Throwable?>()
        val language=AtomicReference("简体中文");val enhancements=AtomicInteger()
        val worker=kotlin.concurrent.thread {
            try {while(!stopped.get())server.accept().use {socket->
                socket.soTimeout=5000
                val input=socket.getInputStream()
                val headers=StringBuilder()
                while(!headers.endsWith("\r\n\r\n")){val byte=input.read();check(byte>=0);headers.append(byte.toChar())}
                val length=headers.lines().first {it.startsWith("content-length:",true)}.substringAfter(':').trim().toInt()
                val bytes=ByteArray(length);var offset=0
                while(offset<length){val count=input.read(bytes,offset,length-offset);check(count>0);offset+=count}
                val payload=JSONObject(bytes.toString(Charsets.UTF_8))
                val content=payload.getJSONArray("messages").getJSONObject(1).get("content")
                val text=if(content is String)content else (content as org.json.JSONArray).let {parts->
                    (0 until parts.length()).mapNotNull {parts.getJSONObject(it).optString("text").takeIf(String::isNotEmpty)}.joinToString("\n")}
                val request=JSONObject(text)
                val result=if(request.has("tb")&&request.has("tl")){
                    check(request.getString("tl")==language.get());enhancements.incrementAndGet()
                    entered.countDown();check(release.await(10,TimeUnit.SECONDS))
                    val chart=if(request.getJSONArray("tb").length()>0)"""[{"tb":[0],"k":"bar","ti":"测量结果","pn":[{"ti":"测量结果","un":"","ctg":["甲","乙"],"sr":[{"nm":"数值","cs":[[0,1,1],[0,2,1]]}]}]}]"""else"[]"
                    val notes=if(request.getJSONArray("f").length()>0)"""[{"i":0,"nt":[{"sy":"x","me":"位置","un":"","ev":"x is the position.","kn":true}]}]"""else"[]"
                    val output="""{"vz":$chart,"nt":$notes}"""
                    if(language.get()=="English")output.replace("测量结果","Measurements").replace("数值","Value").replace("位置","Position").replace("甲","A").replace("乙","B")else output
                }else"""{"g":[],"c":[],"f":[]}"""
                val body=JSONObject().put("choices",org.json.JSONArray().put(JSONObject().put("index",0).put("finish_reason","stop")
                    .put("message",JSONObject().put("role","assistant").put("content",result)))).toString().toByteArray()
                socket.getOutputStream().apply {write("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: ${body.size}\r\nConnection: close\r\n\r\n".toByteArray());write(body);flush()}
            }}catch(e:Throwable){if(!stopped.get())failure.set(e)}
        }
        val file=File(context.cacheDir,"ai-presentation.epub")
        val files=mapOf(
            "mimetype" to "application/epub+zip",
            "META-INF/container.xml" to """<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="package.opf"/></rootfiles></container>""",
            "package.opf" to """<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="uid"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="uid">ai-presentation-ui</dc:identifier><dc:title>AI presentation test</dc:title><dc:language>en</dc:language></metadata><manifest><item id="body" href="body.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="body"/></spine></package>""",
            "body.xhtml" to """<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Media</title></head><body><table><tr><th>Name</th><th>Value</th></tr><tr><td>A</td><td>2</td></tr><tr><td>B</td><td>4</td></tr></table><p><span class="math math-display">x</span></p><p>x is the position.</p></body></html>"""
        )
        ZipOutputStream(file.outputStream()).use {zip->files.forEach {(name,text)->zip.putNextEntry(ZipEntry(name));zip.write(text.toByteArray());zip.closeEntry()}}
        val book=core.importBook(file.absolutePath).single {it.title=="AI presentation test"}
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        var activity:MainActivity?=null;var model:ReaderModel?=null;var providerId:String?=null
        fun await(message:String,condition:()->Boolean){val until=android.os.SystemClock.uptimeMillis()+10000
            while(!condition()){failure.get()?.let {throw AssertionError("Loopback model failed",it)};assertTrue("$message: controls=${model?.state?.value?.frame?.info?.controls}, busy=${model?.state?.value?.busy}, processing=${model?.state?.value?.processing}, enhancing=${model?.state?.value?.enhancing}, error=${model?.state?.value?.aiError}, text=${model?.state?.value?.frame?.info?.regions?.map {it.text}}",android.os.SystemClock.uptimeMillis()<until);android.os.SystemClock.sleep(25)}}
        fun hasProgress(node:AccessibilityNodeInfo?):Boolean=node!=null&&(node.className=="android.widget.ProgressBar"||(0 until node.childCount).any {hasProgress(node.getChild(it))})
        fun hasStop(node:AccessibilityNodeInfo?):Boolean=node!=null&&(node.contentDescription?.toString() in setOf("停止处理","Stop processing")||(0 until node.childCount).any {hasStop(node.getChild(it))})
        try {
            check(preferences.edit().putBoolean("focus",false).commit())
            core.saveProvider(Provider("","custom","AI presentation loopback","http://127.0.0.1:${server.localPort}/v1",listOf("test-model"),false),"inert-test-key",false)
            val provider=core.providers().single {it.name=="AI presentation loopback"};providerId=provider.id
            core.saveReadingOptions(originalReading.copy(translated=false))
            core.saveAiOptions(originalAi.copy(layoutEnabled=true,layoutProvider=provider.id,layoutModel="test-model",targetLanguage="简体中文"))
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity;assertNotNull(activity)
            instrumentation.runOnMainSync {model=ViewModelProvider(activity!!)[ReaderModel::class.java];model!!.open(book)}
            assertTrue("Optional generation starts after base content",entered.await(10,TimeUnit.SECONDS))
            await("Reading stays usable during the optional request"){model!!.state.value.enhancing&&!model!!.state.value.busy&&!model!!.state.value.processing}
            assertNotNull(model!!.state.value.frame)
            instrumentation.runOnMainSync {model!!.toggleChrome()}
            android.os.SystemClock.sleep(350);instrumentation.waitForIdleSync()
            assertFalse("Neither header progress bar may appear",hasProgress(instrumentation.uiAutomation.rootInActiveWindow))
            assertFalse("Optional generation has no header pause/stop entry",hasStop(instrumentation.uiAutomation.rootInActiveWindow))
            release.countDown()
            await("Chart and collapsed formula notes reach the real reader"){model!!.state.value.frame?.info?.controls?.let {controls->controls.any {it.kind=="chart"&&!it.active}&&controls.any {it.kind=="formula"&&!it.active}}==true}
            await("Optional work settles without error"){!model!!.state.value.enhancing&&!model!!.state.value.processing&&!model!!.state.value.busy}
            assertNull(model!!.state.value.aiError)
            val formula=model!!.state.value.frame!!.info.controls.single {it.kind=="formula"}
            val collapsed=model!!.state.value.frame!!.bitmap
            instrumentation.runOnMainSync {model!!.toggleReadingControl(formula)}
            await("Formula notes expand through native pixels"){model!!.state.value.frame?.let {frame->frame.info.controls.any {it.kind=="formula"&&it.active}&&!frame.bitmap.sameAs(collapsed)}==true}
            File(context.cacheDir,"ai-presentation-expanded.png").outputStream().use {model!!.state.value.frame!!.bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG,100,it)}
            val chart=model!!.state.value.frame!!.info.controls.single {it.kind=="chart"}
            val table=model!!.state.value.frame!!.bitmap
            instrumentation.runOnMainSync {model!!.toggleReadingControl(chart)}
            await("The generated chart replaces the table"){model!!.state.value.frame?.let {frame->frame.info.controls.any {it.kind=="chart"&&it.active}&&!frame.bitmap.sameAs(table)}==true}
            assertTrue("Target-language chart titles must produce visible glyphs on Android",hasChartTitle(model!!.state.value.frame!!.bitmap))
            File(context.cacheDir,"ai-presentation-chart.png").outputStream().use {model!!.state.value.frame!!.bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG,100,it)}
            await("First presentation settles"){!model!!.state.value.processing&&!model!!.state.value.enhancing&&!model!!.state.value.busy}
            val generated=enhancements.get();language.set("English")
            instrumentation.runOnMainSync {model!!.saveAiOptions(model!!.state.value.aiOptions!!.copy(targetLanguage="English"))}
            await("Changing language must regenerate metadata for the same page"){enhancements.get()>generated&&model!!.state.value.frame?.info?.controls?.let {controls->controls.any {it.kind=="chart"&&!it.active}&&controls.any {it.kind=="formula"&&!it.active}}==true}
        }finally {
            release.countDown();stopped.set(true);server.close();worker.join(5000)
            instrumentation.runOnMainSync {model?.back();activity?.finish()};instrumentation.removeMonitor(monitor)
            core.saveAiOptions(originalAi);core.saveReadingOptions(originalReading)
            check(preferences.edit().putBoolean("focus",originalFocus).commit())
            providerId?.let {core.deleteProvider(it)}
            if(book.id !in existing)core.removeBook(book.id)
            file.delete()
        }
    }
    private fun hasChartTitle(bitmap:android.graphics.Bitmap):Boolean {
        val pixels=IntArray(bitmap.width*bitmap.height)
        bitmap.getPixels(pixels,0,bitmap.width,0,0,bitmap.width,bitmap.height)
        fun light(pixel:Int)=android.graphics.Color.red(pixel)>230&&android.graphics.Color.green(pixel)>230&&android.graphics.Color.blue(pixel)>230
        val rows=(0 until bitmap.height).filter {y->(0 until bitmap.width).count {x->light(pixels[y*bitmap.width+x])}>bitmap.width*.6}
        if(rows.isEmpty())return false
        val top=rows.first();val height=rows.last()-top
        // The upper part of the chart card contains its title, before plot marks.
        var ink=0
        for(y in top..top+(height*.12).toInt())for(x in bitmap.width/4..bitmap.width*3/4){
            val pixel=pixels[y*bitmap.width+x]
            if(android.graphics.Color.red(pixel)<130&&android.graphics.Color.green(pixel)<130&&android.graphics.Color.blue(pixel)<130)ink++
        }
        return ink>20
    }
}

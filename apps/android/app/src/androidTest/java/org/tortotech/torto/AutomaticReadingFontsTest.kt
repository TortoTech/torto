package org.tortotech.torto

import android.content.Intent
import androidx.lifecycle.ViewModelProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.coroutines.*
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.tortotech.torto.core.ReadingFontDownload
import java.io.File
import java.util.zip.ZipEntry
import java.util.zip.ZipOutputStream

@RunWith(AndroidJUnit4::class)
class AutomaticReadingFontsTest {
    private fun label(vararg labels:String):android.view.accessibility.AccessibilityNodeInfo? {
        val automation=InstrumentationRegistry.getInstrumentation().uiAutomation
        if(android.os.Build.VERSION.SDK_INT>=33)automation.clearCache()
        val queue=java.util.ArrayDeque<android.view.accessibility.AccessibilityNodeInfo>()
        automation.rootInActiveWindow?.let(queue::add)
        while(queue.isNotEmpty()) {
            val node=queue.removeFirst()
            if(node.isVisibleToUser&&(node.text?.toString() in labels||node.contentDescription?.toString() in labels))return node
            for(index in 0 until node.childCount)node.getChild(index)?.let(queue::add)
        }
        return null
    }
    private fun fixture():File {
        val context=InstrumentationRegistry.getInstrumentation().targetContext
        val id=java.util.UUID.randomUUID().toString()
        val file=File(context.cacheDir,"font-notice-$id.epub")
        val body=(0 until 30).joinToString(""){"<p id='p$it'>Passage $it. The reading position stays attached to this paragraph when downloaded fonts are applied. We keep reading while the resources arrive in the background.</p>"}
        val files=mapOf(
            "mimetype" to "application/epub+zip",
            "META-INF/container.xml" to """<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="package.opf"/></rootfiles></container>""",
            "package.opf" to """<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="uid"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="uid">$id</dc:identifier><dc:title>Font notice $id</dc:title><dc:language>en</dc:language></metadata><manifest><item id="body" href="body.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="body"/></spine></package>""",
            "body.xhtml" to """<html xmlns="http://www.w3.org/1999/xhtml"><body>$body</body></html>""",
        )
        ZipOutputStream(file.outputStream()).use {zip->files.forEach {(name,text)->zip.putNextEntry(ZipEntry(name));zip.write(text.toByteArray());zip.closeEntry()}}
        return file
    }
    @Test fun backgroundDownloadDoesNotBlockTheShelfOrForegroundOperations()=runBlocking {
        val instrumentation=InstrumentationRegistry.getInstrumentation()
        val original=ReadingFonts.loader
        val started=CompletableDeferred<Unit>();val finish=CompletableDeferred<Unit>()
        val calls=java.util.concurrent.atomic.AtomicInteger()
        val previouslyStarted=ReadingFonts.state.value.revision>0
        val core=NativeRuntime.core(instrumentation.targetContext)
        val originalBooks=core.books().map {it.id}.toSet()
        val originalAi=core.aiOptions();val originalReading=core.readingOptions()
        core.saveAiOptions(originalAi.copy(layoutEnabled=false))
        core.saveReadingOptions(originalReading.copy(translated=false))
        val fixture=fixture()
        val book=core.importBook(fixture.absolutePath).single {it.id !in originalBooks}
        ReadingFonts.loader={calls.incrementAndGet();started.complete(Unit);finish.await();ReadingFontDownload(3u,true,null)}
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        var activity:MainActivity?=null
        try {
            instrumentation.targetContext.startActivity(Intent(instrumentation.targetContext,MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TASK))
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity
            assertNotNull(activity)
            lateinit var model:ReaderModel
            instrumentation.runOnMainSync {model=ViewModelProvider(activity!!)[ReaderModel::class.java]}
            withTimeout(15000){while(!model.state.value.loaded||model.state.value.busy)delay(20)}
            // The fresh-process case must start from the first displayed shelf
            // frame. Full-suite runs may already have completed an offline batch.
            if(previouslyStarted)ReadingFonts.start(instrumentation.targetContext,automatic=false)
            withTimeout(15000){started.await()}
            assertTrue(model.state.value.loaded)
            assertFalse(model.state.value.busy)
            instrumentation.runOnMainSync {model.downloadFonts();model.loadStatistics()}
            withTimeout(15000){while(model.state.value.statistics==null||model.state.value.busy)delay(20)}
            assertTrue(ReadingFonts.state.value.running)
            assertEquals(1,calls.get())
            instrumentation.runOnMainSync {model.open(book)}
            withTimeout(15000){while(model.state.value.frame==null||model.state.value.busy)delay(20)}
            val turn=model.state.value.turnCompletion
            instrumentation.runOnMainSync {model.turn(true)}
            withTimeout(15000){while(model.state.value.turnCompletion==turn||model.state.value.busy)delay(20)}
            instrumentation.runOnMainSync {activity!!.recreate()}
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity
            assertNotNull(activity)
            instrumentation.waitForIdleSync()
            instrumentation.runOnMainSync {assertSame(model,ViewModelProvider(activity!!)[ReaderModel::class.java])}
            delay(250)
            withTimeout(15000){while(model.state.value.busy)delay(20)}
            assertTrue(ReadingFonts.state.value.running)
            assertEquals(1,calls.get())
            val before=model.state.value.frame!!
            finish.complete(Unit)
            withTimeout(15000){while(model.state.value.fontsDownloading||!model.state.value.fontsReady||model.state.value.notice==null)delay(20)}
            assertEquals("新字体已下载完成",model.state.value.notice)
            assertSame("Downloading must not replace the current reader frame",before,model.state.value.frame)
            withTimeout(3000){while(label("应用","Apply")==null)delay(20)}
            var action=label("应用","Apply")
            while(action!=null&&!action.isClickable)action=action.parent
            assertNotNull(action)
            instrumentation.uiAutomation.takeScreenshot()?.let {bitmap->
                File(instrumentation.targetContext.cacheDir,"font-notice-action.png").outputStream().use {bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG,100,it)}
                bitmap.recycle()
            }
            assertTrue(action!!.performAction(android.view.accessibility.AccessibilityNodeInfo.ACTION_CLICK))
            withTimeout(15000){while(model.state.value.fontApplyAvailable||model.state.value.busy||model.state.value.notice!=null)delay(20)}
            val after=model.state.value.frame!!.info
            assertEquals(before.info.section,after.section)
            assertEquals(before.info.page,after.page)
            assertEquals(before.info.focusIndex,after.focusIndex)
            assertFalse(model.state.value.busy)
            assertNull(model.state.value.error)
        }finally {
            finish.complete(Unit)
            withTimeout(5000){while(ReadingFonts.state.value.running)delay(10)}
            ReadingFonts.loader=original
            instrumentation.runOnMainSync {activity?.finish()}
            instrumentation.removeMonitor(monitor)
            core.removeBook(book.id);fixture.delete()
            core.saveAiOptions(originalAi);core.saveReadingOptions(originalReading)
        }
    }
    @Test fun fontNoticeExpiresAfterFiveSecondsAcrossActivityRecreation()=runBlocking {
        val instrumentation=InstrumentationRegistry.getInstrumentation()
        val original=ReadingFonts.loader
        val previouslyStarted=ReadingFonts.state.value.revision>0
        ReadingFonts.loader={ReadingFontDownload(1u,true,null)}
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        var activity:MainActivity?=null
        try {
            instrumentation.targetContext.startActivity(Intent(instrumentation.targetContext,MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TASK))
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity;assertNotNull(activity)
            lateinit var model:ReaderModel
            instrumentation.runOnMainSync {model=ViewModelProvider(activity!!)[ReaderModel::class.java]}
            withTimeout(15000){while(!model.state.value.loaded||model.state.value.busy)delay(20)}
            if(previouslyStarted)instrumentation.runOnMainSync {model.downloadFonts()}
            withTimeout(5000){while(label("新字体已下载完成","New fonts downloaded")==null)delay(20)}
            val shown=android.os.SystemClock.uptimeMillis()
            delay(1500)
            instrumentation.runOnMainSync {activity!!.recreate()}
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity;assertNotNull(activity)
            while(android.os.SystemClock.uptimeMillis()-shown<4200)delay(20)
            assertNotNull("Notice must remain visible before five seconds",label("新字体已下载完成","New fonts downloaded"))
            withTimeout(2000){while(label("新字体已下载完成","New fonts downloaded")!=null||model.state.value.notice!=null)delay(20)}
            assertTrue(android.os.SystemClock.uptimeMillis()-shown<6200)
        }finally {
            ReadingFonts.loader=original
            instrumentation.runOnMainSync {activity?.finish()}
            instrumentation.removeMonitor(monitor)
        }
    }
}

package org.tortotech.torto

import android.graphics.Bitmap
import android.os.SystemClock
import android.view.MotionEvent
import android.view.ViewConfiguration
import android.view.accessibility.AccessibilityNodeInfo
import androidx.lifecycle.ViewModelProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.coroutines.runBlocking
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.tortotech.torto.core.PageInfo
import org.tortotech.torto.core.FocusTarget
import java.io.File
import java.util.concurrent.atomic.AtomicInteger

/** Header and system back leave reading even after an in-book jump. */
@RunWith(AndroidJUnit4::class)
class ReaderBackNavigationTest {
    private val instrumentation get()=InstrumentationRegistry.getInstrumentation()
    private fun await(message:String,condition:()->Boolean){val until=SystemClock.uptimeMillis()+8000;while(!condition()){assertTrue(message,SystemClock.uptimeMillis()<until);SystemClock.sleep(25)}}
    private fun backNode():AccessibilityNodeInfo? {
        instrumentation.uiAutomation.clearCache()
        val queue=java.util.ArrayDeque<AccessibilityNodeInfo>()
        instrumentation.uiAutomation.rootInActiveWindow?.let(queue::add)
        while(queue.isNotEmpty()){
            val node=queue.removeFirst()
            if(node.isVisibleToUser&&(node.contentDescription?.toString() in listOf("返回","Back")))return node
            for(i in 0 until node.childCount)node.getChild(i)?.let(queue::add)
        }
        return null
    }
    private fun fixture(path:File){
        val files=mapOf(
            "mimetype" to "application/epub+zip",
            "META-INF/container.xml" to """<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="book.opf"/></rootfiles></container>""",
            "book.opf" to """<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="id">new-bug-audit</dc:identifier><dc:title>New Bug Audit</dc:title><dc:language>en</dc:language></metadata><manifest><item id="body" href="body.xhtml" media-type="application/xhtml+xml"/><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/></manifest><spine><itemref idref="body"/></spine></package>""",
            "body.xhtml" to """<html xmlns="http://www.w3.org/1999/xhtml"><body><h1 id="one">One</h1><p>First paragraph.</p><h1 id="two">Two</h1><p>Last paragraph.</p></body></html>""",
            "nav.xhtml" to """<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="body.xhtml#one">One</a></li><li><a href="body.xhtml#two">Two</a></li></ol></nav></body></html>"""
        )
        java.util.zip.ZipOutputStream(path.outputStream()).use {zip->files.forEach {(name,text)->zip.putNextEntry(java.util.zip.ZipEntry(name));zip.write(text.toByteArray());zip.closeEntry()}}
    }
    @Test fun headerAndSystemBackReturnToShelfAfterInBookNavigation():Unit=runBlocking {
        val context=instrumentation.targetContext;val core=NativeRuntime.core(context)
        val originalAi=core.aiOptions();val before=core.books().map {it.id}.toSet()
        val file=File(context.cacheDir,"new-bug-audit.epub");fixture(file)
        val book=core.importBook(file.absolutePath).single {it.title=="New Bug Audit"}
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        var activity:MainActivity?=null;var model:ReaderModel?=null
        try {
            core.saveAiOptions(originalAi.copy(layoutEnabled=false))
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity;assertNotNull(activity)
            instrumentation.runOnMainSync {model=ViewModelProvider(activity!!)[ReaderModel::class.java]}
            await("model loaded"){model!!.state.value.loaded&&!model!!.state.value.busy}
            for(system in listOf(false,true)) {
                instrumentation.runOnMainSync {model!!.open(book)}
                await("book loaded"){model!!.state.value.frame!=null&&!model!!.state.value.busy}
                instrumentation.runOnMainSync {model!!.followLink("body.xhtml#two")}
                await("book history exists"){model!!.state.value.frame?.info?.canGoBack==true&&!model!!.state.value.busy}
                if(system)android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("input keyevent 4")).use {it.readBytes()}
                else {
                    instrumentation.runOnMainSync {model!!.toggleChrome()}
                    await("header back visible"){backNode()!=null}
                    var button=backNode()
                    while(button!=null&&!button.isClickable)button=button.parent
                    assertNotNull(button)
                    assertTrue(button!!.performAction(AccessibilityNodeInfo.ACTION_CLICK))
                }
                await("one back returns directly to the shelf"){model!!.state.value.active==null&&!model!!.state.value.busy}
                assertNull(model!!.state.value.frame)

            }
            instrumentation.runOnMainSync {model!!.open(book)}
            await("reader reopens"){model!!.state.value.frame!=null&&!model!!.state.value.busy}
            repeat(3){
                if(!model!!.state.value.completionPage){
                    val completed=model!!.state.value.turnCompletion
                    instrumentation.runOnMainSync {model!!.turnUnit(true)}
                    await("turn completes"){model!!.state.value.turnCompletion>completed&&!model!!.state.value.busy}
                }
            }
            assertTrue(model!!.state.value.completionPage)
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("input keyevent 4")).use {it.readBytes()}
            await("system back on completion also exits to the shelf"){model!!.state.value.active==null&&!model!!.state.value.busy}
        }finally {
            instrumentation.runOnMainSync {model?.back();activity?.finish()};instrumentation.removeMonitor(monitor)
            core.saveAiOptions(originalAi);if(book.id !in before)core.removeBook(book.id);file.delete()
        }
    }
}

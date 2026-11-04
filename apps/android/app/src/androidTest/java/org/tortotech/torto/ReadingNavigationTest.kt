package org.tortotech.torto

import android.graphics.Rect
import android.os.Bundle
import android.view.View
import android.view.ViewGroup
import android.view.accessibility.AccessibilityNodeInfo
import androidx.lifecycle.ViewModelProvider
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.Modifier
import org.tortotech.torto.core.BookSummary
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.coroutines.runBlocking
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File

@RunWith(AndroidJUnit4::class)
class ReadingNavigationTest {
    private val instrumentation get()=InstrumentationRegistry.getInstrumentation()
    private fun await(message:String,condition:()->Boolean){
        val deadline=android.os.SystemClock.uptimeMillis()+6000
        while(!condition()){assertTrue(message,android.os.SystemClock.uptimeMillis()<deadline);android.os.SystemClock.sleep(35)}
    }
    private fun node(predicate:(AccessibilityNodeInfo)->Boolean):AccessibilityNodeInfo?{
        val root=instrumentation.uiAutomation.rootInActiveWindow ?: return null
        val queue=java.util.ArrayDeque<AccessibilityNodeInfo>();queue.add(root)
        var checked=0
        while(queue.isNotEmpty()&&checked++<1500){val item=queue.removeFirst();if(item.isVisibleToUser&&predicate(item))return item
            for(index in 0 until item.childCount)item.getChild(index)?.let(queue::add)
        }
        return null
    }
    private fun label(vararg names:String)=node {it.contentDescription?.toString() in names||it.text?.toString() in names}
    private fun click(item:AccessibilityNodeInfo?){var target=item;while(target!=null&&!target.isClickable)target=target.parent;assertNotNull(target);assertTrue(target!!.performAction(AccessibilityNodeInfo.ACTION_CLICK))}
    private fun reader(view:View):ReaderView?=if(view is ReaderView)view else if(view is ViewGroup)(0 until view.childCount).firstNotNullOfOrNull {reader(view.getChildAt(it))}else null

    private fun fixture(file:File){
        val body="<h1 id='one'>Chapter One</h1><p>Independent paragraph for navigation and search.</p>"+(0..15).joinToString(""){"<p>Paragraph $it keeps source locations stable while controls show and hide.</p>"}+"<h2 id='two'>Chapter Two</h2><p>Another chapter in the same source.</p><h2 id='appendix'>Appendix</h2><p>End notes.</p>"
        val files=mapOf(
            "mimetype" to "application/epub+zip",
            "META-INF/container.xml" to """<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="package.opf"/></rootfiles></container>""",
            "package.opf" to """<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="uid"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="uid">reader-navigation-ui</dc:identifier><dc:title>Navigation UI fixture</dc:title><dc:language>en</dc:language></metadata><manifest><item id="body" href="body.xhtml" media-type="application/xhtml+xml"/><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/></manifest><spine><itemref idref="body"/></spine></package>""",
            "body.xhtml" to """<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Navigation</title></head><body>$body</body></html>""",
            "nav.xhtml" to """<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><head><title>Contents</title></head><body><nav epub:type="toc"><ol><li><a href="body.xhtml">Part One</a><ol><li><a href="body.xhtml#one">Chapter One</a></li><li><a href="body.xhtml#two">Chapter Two</a></li></ol></li><li><a href="body.xhtml#appendix">Appendix</a></li></ol></nav></body></html>"""
        )
        java.util.zip.ZipOutputStream(file.outputStream()).use {zip->files.forEach {(name,text)->zip.putNextEntry(java.util.zip.ZipEntry(name));zip.write(text.toByteArray());zip.closeEntry()}}
    }
    private fun screenshot(name:String){instrumentation.uiAutomation.takeScreenshot()?.let {bitmap->File(instrumentation.targetContext.cacheDir,name).outputStream().use {bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG,100,it)};bitmap.recycle()}}

    @Test fun shortBusyUpdatesKeepToolbarButtonsEnabledAndChatInPlace(){
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        var activity:MainActivity?=null
        val screen=mutableStateOf(ScreenState(active=BookSummary(0.0,0uL,0uL,"chrome-ui","Chrome",emptyList()),language="en"))
        try {
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity;assertNotNull(activity)
            instrumentation.runOnMainSync {activity!!.setContent {TortoTheme(screen.value){
                val blocked=loadingVisible(screen.value.busy)
                val processing=loadingVisible(screen.value.processing)
                Box(Modifier.fillMaxSize()){ReadingChrome(screen.value,blocked,processing,null,{},{},{},{},{})}
            }}}
            await("toolbar ready"){label("Reading assistant")!=null}
            fun button(name:String):AccessibilityNodeInfo {var found=label(name);while(found!=null&&!found.isClickable&&found.className!="android.widget.Button")found=found.parent;return found!!}
            fun bounds(name:String)=Rect().also {button(name).getBoundsInScreen(it)}
            val chat=bounds("Reading assistant")
            instrumentation.runOnMainSync {screen.value=screen.value.copy(busy=true)}
            android.os.SystemClock.sleep(50)
            for(name in listOf("Reading assistant","Contents and search","Notes and highlights","Selection mode","Typography","Toggle light/dark","Turn translation on"))assertTrue("Short reading updates keep $name enabled",button(name).isEnabled)
            instrumentation.runOnMainSync {screen.value=screen.value.copy(busy=false,processing=true)}
            await("genuine AI processing gets a stop control"){label("Stop processing")!=null}
            assertEquals("AI controls must not shift chat",chat,bounds("Reading assistant"))
            instrumentation.runOnMainSync {screen.value=screen.value.copy(busy=true)}
            await("long operations still disable conflicting controls"){!button("Reading assistant").isEnabled}
            instrumentation.runOnMainSync {screen.value=screen.value.copy(busy=false,processing=false)}
            await("controls restore after work"){button("Reading assistant").isEnabled}
        }finally {instrumentation.runOnMainSync {activity?.finish()};instrumentation.removeMonitor(monitor)}
    }

    @Test fun navigationSearchAndChromeKeepTheReadingViewportStable():Unit=runBlocking {
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        var activity:MainActivity?=null
        var model:ReaderModel?=null
        val context=instrumentation.targetContext
        val core=NativeRuntime.core(context)
        val originalAi=core.aiOptions();val originalReading=core.readingOptions()
        val before=core.books().map {it.id}.toSet()
        val fixture=File(context.cacheDir,"navigation-ui.epub")
        fixture(fixture)
        val book=core.importBook(fixture.absolutePath).first {it.title=="Navigation UI fixture"}
        try {
            core.saveAiOptions(originalAi.copy(layoutEnabled=false));core.saveReadingOptions(originalReading.copy(translated=false))
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity;assertNotNull(activity)
            instrumentation.runOnMainSync {model=ViewModelProvider(activity!!)[ReaderModel::class.java]}
            await("model loads"){model!!.state.value.loaded&&!model!!.state.value.busy}
            instrumentation.runOnMainSync {model!!.open(book)}
            await("reader loads"){model!!.state.value.frame!=null&&!model!!.state.value.busy}
            android.os.SystemClock.sleep(350)
            var size=0 to 0
            instrumentation.runOnMainSync {reader(activity!!.window.decorView)!!.let {size=it.width to it.height}}
            assertTrue(size.second>0)
            await("floating translation control is visible"){label("开启翻译","Turn translation on")!=null}
            assertNull("Reading progress labels were removed",node {it.text?.toString()?.let {s->s.contains('%')&&s.contains('/')}==true})
            instrumentation.runOnMainSync {model!!.toggleChrome()}
            await("controls hide together"){label("目录与搜索","Contents and search")==null&&label("开启翻译","Turn translation on")==null}
            android.os.SystemClock.sleep(300)
            instrumentation.runOnMainSync {reader(activity!!.window.decorView)!!.let {assertEquals(size,it.width to it.height)}}
            instrumentation.runOnMainSync {model!!.toggleChrome()}
            await("controls restore"){label("目录与搜索","Contents and search")!=null}
            instrumentation.runOnMainSync {reader(activity!!.window.decorView)!!.let {assertEquals(size,it.width to it.height)}}
            screenshot("navigation-reader.png")
            click(label("目录与搜索","Contents and search"))
            await("combined navigation drawer opens"){label("搜本书","Search book")!=null}
            await("Footer remains available"){label("目录与搜索","Contents and search")!=null}
            await("current chapter is identified by the core"){label("当前读到","Reading here")!=null}
            android.os.SystemClock.sleep(250)
            screenshot("navigation-contents.png")
            click(label("收起章节","Collapse chapter"))
            await("parent collapses its children"){label("Chapter One")==null}
            click(label("展开章节","Expand chapter"))
            await("parent expands its children"){label("Chapter One")!=null}
            click(label("搜本书","Search book"))
            await("search field takes focus"){node {it.className=="android.widget.EditText"}!=null}
            val args=Bundle().apply {putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE,"Independent")}
            assertTrue(node {it.className=="android.widget.EditText"}!!.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT,args))
            await("search action available"){label("搜索","Search")?.isEnabled==true}
            click(label("搜索","Search"))
            await("search uses shared native index"){model!!.state.value.hits.isNotEmpty()}
            await("results shown in drawer"){node {it.text?.toString()?.contains("Independent")==true&&it.className!="android.widget.EditText"}!=null}
            click(node {it.text?.toString()?.contains("Independent")==true&&it.className!="android.widget.EditText"})
            await("result closes drawer"){label("搜本书","Search book")==null}
            await("floating control restores"){label("开启翻译","Turn translation on")!=null}
            instrumentation.runOnMainSync {model!!.back()}
            await("home restores"){model!!.state.value.active==null&&!model!!.state.value.busy}
            screenshot("navigation-home.png")
            val home=node {it.text?.toString() in setOf("书架","Library")&&it.isVisibleToUser&&android.graphics.Rect().also(it::getBoundsInScreen).top>context.resources.displayMetrics.heightPixels/2};assertNotNull(home)
            var tab=home;while(tab!=null&&!tab.isClickable&&!tab.isSelected)tab=tab.parent
            val bounds=Rect();tab!!.getBoundsInScreen(bounds)
            val density=context.resources.displayMetrics.density
            assertTrue("Home tab retains a 48dp touch target",bounds.height()/density>=48f)
            assertTrue("Home navigation is compact",bounds.height()/density<65f)
        }finally {
            instrumentation.runOnMainSync {model?.back();activity?.finish()};instrumentation.removeMonitor(monitor)
            core.saveAiOptions(originalAi);core.saveReadingOptions(originalReading)
            if(book.id !in before)core.removeBook(book.id)
            fixture.delete()
        }
    }
}

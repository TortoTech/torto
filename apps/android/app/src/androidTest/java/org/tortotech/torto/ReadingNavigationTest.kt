package org.tortotech.torto

import android.graphics.Rect
import android.os.Bundle
import android.view.View
import android.view.ViewGroup
import android.view.accessibility.AccessibilityNodeInfo
import androidx.core.view.WindowInsetsCompat
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
    private fun node(includeVirtualText:Boolean=false,predicate:(AccessibilityNodeInfo)->Boolean):AccessibilityNodeInfo?{
        if(android.os.Build.VERSION.SDK_INT>=33)instrumentation.uiAutomation.clearCache()
        val root=instrumentation.uiAutomation.rootInActiveWindow ?: return null
        val queue=java.util.ArrayDeque<AccessibilityNodeInfo>();queue.add(root)
        var checked=0
        while(queue.isNotEmpty()&&checked++<1500){val item=queue.removeFirst();if(item.isVisibleToUser&&(item.isImportantForAccessibility||includeVirtualText&&item.className=="android.widget.TextView")&&predicate(item))return item
            for(index in 0 until item.childCount)item.getChild(index)?.let(queue::add)
        }
        return null
    }
    private fun label(vararg names:String)=node {it.contentDescription?.toString() in names||it.text?.toString() in names}
    private fun clickable(item:AccessibilityNodeInfo?):AccessibilityNodeInfo? {var target=item;while(target!=null&&!target.isClickable)target=target.parent;return target}
    private fun click(item:AccessibilityNodeInfo?){val target=clickable(item);assertNotNull(target);assertTrue(target!!.performAction(AccessibilityNodeInfo.ACTION_CLICK))}
    private fun reader(view:View):ReaderView?=if(view is ReaderView)view else if(view is ViewGroup)(0 until view.childCount).firstNotNullOfOrNull {reader(view.getChildAt(it))}else null

    private fun fixture(file:File){
        val body="<h1 id='one'>Chapter One</h1><p>Independent paragraph for navigation and search. This is a second sentence.</p>"+(0..15).joinToString(""){"<p>Paragraph $it keeps source locations stable while controls show and hide. A second sentence remains in this paragraph.</p>"}+"<h2 id='two'>Chapter Two</h2><p>Another chapter in the same source.</p><h2 id='appendix'>Appendix</h2><p>End notes.</p>"
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

    @Test fun referenceSheetUsesAlignedMarkersAndContentHeight():Unit=runBlocking {
        val context=instrumentation.targetContext
        val core=NativeRuntime.core(context)
        val before=core.books().map {it.id}.toSet()
        val originalAi=core.aiOptions();val originalReading=core.readingOptions()
        val fixture=File(context.cacheDir,"note-sheet-ui.epub")
        instrumentation.context.assets.open("reader-interactions.epub").use {input->fixture.outputStream().use {input.copyTo(it)}}
        val book=core.importBook(fixture.absolutePath).first {it.title=="阅读交互测试"}
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        var activity:MainActivity?=null;var model:ReaderModel?=null;var previous:ScreenState?=null
        try {
            core.saveAiOptions(originalAi.copy(layoutEnabled=false));core.saveReadingOptions(originalReading.copy(translated=false))
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity;assertNotNull(activity)
            instrumentation.runOnMainSync {model=ViewModelProvider(activity!!)[ReaderModel::class.java]}
            await("model loads"){model!!.state.value.loaded&&!model!!.state.value.busy}
            previous=model!!.state.value
            instrumentation.runOnMainSync {model!!.open(book)}
            await("note fixture opens"){model!!.state.value.frame!=null&&!model!!.state.value.busy}
            instrumentation.runOnMainSync {model!!.focus(false)}
            await("classic note targets are stable"){!model!!.state.value.focus&&!model!!.state.value.busy}
            instrumentation.runOnMainSync {model!!.navigate("body.xhtml")}
            await("fixture starts at its reference paragraph"){!model!!.state.value.busy}
            for(dark in listOf(false,true)){
                instrumentation.runOnMainSync {model!!.configure(20f,true,dark)}
                await("theme settles"){!model!!.state.value.busy&&model!!.state.value.dark==dark}
                val frame=model!!.state.value.frame!!
                val bitmap=frame.bitmap;val density=context.resources.displayMetrics.density
                val height=minOf(bitmap.height,(180*density).toInt())
                val pixels=IntArray(bitmap.width*height);bitmap.getPixels(pixels,0,bitmap.width,0,0,bitmap.width,height)
                val accent=org.tortotech.torto.core.referenceColor(dark).toInt()
                var left=bitmap.width;var top=height;var right=0;var bottom=0
                pixels.forEachIndexed {index,pixel->if(pixel==accent){val x=index%bitmap.width;val y=index/bitmap.width;left=minOf(left,x);right=maxOf(right,x);top=minOf(top,y);bottom=maxOf(bottom,y)}}
                assertTrue("Reference markers use the shared desktop accent",left<=right&&top<=bottom)
                instrumentation.runOnMainSync {model!!.tap((left+right)/2f/density,(top+bottom)/2f/density)}
                android.os.SystemClock.sleep(600)
                await("numbered note opens"){model!!.state.value.interaction?.kind=="note"}
                assertEquals("1",model!!.state.value.interaction!!.marker)
                assertNotNull(model!!.state.value.interaction!!.indicator)
                await("native note body loads"){node(true) {it.text?.toString()?.contains("Note continuation.")==true}!=null}
                val marker=Rect().also {label("1")!!.getBoundsInScreen(it)}
                val body=Rect().also {node(true) {it.text?.toString()?.contains("multi-paragraph note.")==true}!!.getBoundsInScreen(it)}
                val tail=Rect().also {node(true) {it.text?.toString()?.contains("Note continuation.")==true}!!.getBoundsInScreen(it)}
                val pane=Rect().also {node {it.paneTitle?.toString() in setOf("脚注内容","Footnote content")}!!.getBoundsInScreen(it)}
                assertTrue("The number has its own gutter",marker.right<=body.left+2)
                assertTrue("Short notes must not reserve a blank 520dp body",pane.bottom-tail.bottom<48*density)
                assertNull(label("脚注","Footnote"));assertNull(label("关闭引用","Close reference"));assertNull(label("查看原文位置","Go to original note"))
                assertEquals(frame.rasterId,model!!.state.value.frame!!.rasterId)
                screenshot(if(dark)"reference-note-dark.png"else"reference-note-light.png")
                android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("input keyevent 4")).use {it.readBytes()}
                await("note dismisses without moving the book"){model!!.state.value.interaction==null}
                assertEquals(frame.info.progress,model!!.state.value.frame!!.info.progress,.000001)
            }
        }finally {
            previous?.let {saved->instrumentation.runOnMainSync {model?.dismissInteraction();model?.configure(saved.fontSize,saved.unified,saved.dark);model?.focus(saved.focus)}
                await("reading settings restore"){model?.state?.value?.busy==false}}
            instrumentation.runOnMainSync {model?.back();activity?.finish()};instrumentation.removeMonitor(monitor)
            core.saveAiOptions(originalAi);core.saveReadingOptions(originalReading)
            if(book.id !in before)core.removeBook(book.id)
            fixture.delete()
        }
    }

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
            val split=bounds("Split current paragraph")
            val translation=bounds("Turn translation on")
            assertTrue("Floating actions are AI / sentences / translation",chat.right<split.left&&split.right<translation.left)
            assertEquals(chat.top,split.top);assertEquals(split.top,translation.top)
            assertEquals("Circular controls have square bounds",chat.width(),chat.height())
            assertFalse("No current focus means sentence action is disabled",button("Split current paragraph").isEnabled)
            android.os.SystemClock.sleep(350)
            screenshot("reading-floating-actions.png")
            instrumentation.runOnMainSync {screen.value=screen.value.copy(busy=true)}
            android.os.SystemClock.sleep(50)
            assertTrue("The first button stays usable during a short operation",button("Reading assistant").isEnabled)
            instrumentation.runOnMainSync {screen.value=screen.value.copy(busy=false)}
            instrumentation.waitForIdleSync()
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
            instrumentation.runOnMainSync {
                val root=activity!!.window.decorView
                val insets=WindowInsetsCompat.toWindowInsetsCompat(root.rootWindowInsets)
                    .getInsetsIgnoringVisibility(WindowInsetsCompat.Type.displayCutout() or WindowInsetsCompat.Type.navigationBars())
                assertTrue("The content must use the full safe viewport without toolbar bands",size.second>=root.height-insets.top-insets.bottom-2)
            }
            assertFalse("Books open with controls hidden like the old app",model!!.state.value.chromeVisible)
            fun statusVisible()=WindowInsetsCompat.toWindowInsetsCompat(activity!!.window.decorView.rootWindowInsets).isVisible(WindowInsetsCompat.Type.statusBars())
            await("status bar hides with the header"){!statusVisible()}
            val originalFocus=model!!.state.value.focus
            instrumentation.runOnMainSync {model!!.focus(true)}
            await("focus is ready"){model!!.state.value.focus&&!model!!.state.value.busy&&model!!.state.value.frame!!.info.focusCount>0u}
            val active=model!!.state.value.frame!!.info.focusBounds.first()
            val raster=model!!.state.value.frame!!.rasterId
            instrumentation.runOnMainSync {model!!.tap(active.x+2f,active.y+active.height/2)}
            await("tapping the active block reveals chrome"){model!!.state.value.chromeVisible&&!model!!.state.value.busy}
            await("status bar returns with the header"){statusVisible()}
            assertEquals("Toggling chrome reuses native page pixels",raster,model!!.state.value.frame!!.rasterId)
            assertTrue("Native focus exposes a local sentence control",model!!.state.value.frame!!.info.controls.any {it.kind=="sentences"})
            await("current paragraph can be split"){label("当前段落按句分段","Split current paragraph")!=null}
            click(label("当前段落按句分段","Split current paragraph"))
            await("only current paragraph is split"){!model!!.state.value.busy&&model!!.state.value.frame!!.info.controls.any {it.kind=="sentences"&&it.active}}
            await("action switches to restore"){label("取消当前段落分句","Restore current paragraph")!=null}
            instrumentation.waitForIdleSync()
            android.os.SystemClock.sleep(250)
            screenshot("reading-focused-sentences.png")
            click(label("取消当前段落分句","Restore current paragraph"))
            await("paragraph is restored"){!model!!.state.value.busy&&model!!.state.value.frame!!.info.controls.any {it.kind=="sentences"&&!it.active}}
            instrumentation.runOnMainSync {model!!.tap(active.x+2f,active.y+active.height/2)}
            await("tapping it again hides chrome"){!model!!.state.value.chromeVisible&&!model!!.state.value.busy}
            await("status bar hides again"){!statusVisible()}
            repeat(5){
                val distance=model!!.state.value.frame!!.info.scrollDistance
                instrumentation.runOnMainSync {model!!.turn(true)}
                await("focused content advances across the full viewport"){!model!!.state.value.busy&&model!!.state.value.frame!!.info.scrollDistance!=distance}
                android.os.SystemClock.sleep(400)
            }
            await("full-screen reading is settled"){!model!!.state.value.readerMoving&&!statusVisible()}
            screenshot("navigation-fullscreen.png")
            val classicLayout=model!!.state.value.unified
            instrumentation.runOnMainSync {model!!.toggleChrome()}
            await("typography is available"){label("字体排版","Typography")!=null}
            click(label("字体排版","Typography"))
            await("focus-specific options are shown"){label("专注样式","Focus style")!=null}
            assertNull("Focus always uses unified layout",label("统一版式","Unified layout"))
            assertNull("Focus always scrolls",label("滚动专注","Scrolling focus"))
            assertNull("Sentence splitting is a local floating action",label("按句分段显示","Split sentences onto separate lines"))
            assertTrue(model!!.state.value.frame!!.info.isScroll)
            instrumentation.runOnMainSync {model!!.focus(false)}
            await("classic offers the layout choice"){label("统一版式","Unified layout")!=null&&!model!!.state.value.busy}
            assertNull("Classic does not offer focus styles",label("专注样式","Focus style"))
            assertNull("Classic does not offer focus scrolling",label("滚动专注","Scrolling focus"))
            assertEquals("Switching modes preserves the classic choice",classicLayout,model!!.state.value.unified)
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("input keyevent 4")).use {it.readBytes()}
            await("typography closes"){label("专注模式","Focus mode")==null}
            instrumentation.runOnMainSync {model!!.hideChrome();model!!.focus(originalFocus)}
            await("original reading mode restores"){model!!.state.value.focus==originalFocus&&!model!!.state.value.busy}
            instrumentation.runOnMainSync {model!!.toggleChrome()}
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
            await("parent collapses its children"){label("Chapter One")==null&&label("展开章节","Expand chapter")!=null}
            fun footerBounds(vararg names:String)=Rect().also {bounds->
                val icon=node {it.contentDescription?.toString() in names}
                clickable(icon)!!.getBoundsInScreen(bounds)
            }
            fun tapFooter(vararg names:String){
                await("footer action is enabled"){clickable(node {it.contentDescription?.toString() in names})?.isEnabled==true}
                val bounds=footerBounds(*names);val clock=android.os.SystemClock.uptimeMillis()
                val down=android.view.MotionEvent.obtain(clock,clock,0,bounds.exactCenterX(),bounds.exactCenterY(),0)
                val up=android.view.MotionEvent.obtain(clock,clock+60,1,bounds.exactCenterX(),bounds.exactCenterY(),0)
                instrumentation.sendPointerSync(down);instrumentation.sendPointerSync(up);down.recycle();up.recycle()
            }
            val footer=footerBounds("目录与搜索","Contents and search")
            for((names,paneNames,shot) in listOf(
                Triple(arrayOf("字体排版","Typography"),setOf("字体排版面板","Typography panel"),"panel-typography.png"),
                Triple(arrayOf("文字选择模式","Selection mode"),setOf("选择模式面板","Selection mode panel"),"panel-selection.png"),
                Triple(arrayOf("批注与高亮","Notes and highlights"),setOf("批注与高亮面板","Notes and highlights panel"),"panel-annotations.png")
            )){
                tapFooter(*names)
                await("the footer switches panels without a modal window"){node {it.paneTitle?.toString() in paneNames}!=null&&!model!!.state.value.busy}
                await("panels hide the reading header and status bar"){label("阅读助手","Reading assistant")==null&&!statusVisible()}
                assertEquals("The footer stays in exactly the same place",footer,footerBounds("目录与搜索","Contents and search"))
                val pane=Rect().also {node {it.paneTitle?.toString() in paneNames}!!.getBoundsInScreen(it)}
                assertTrue("A panel cannot cover the footer's touch targets",pane.bottom<=footer.top+2)
                instrumentation.runOnMainSync {reader(activity!!.window.decorView)!!.let {assertEquals("Panel changes keep native pagination stable",size,it.width to it.height)}}
                val previousDark=model!!.state.value.dark
                for(dark in listOf(false,true)){
                    if(model!!.state.value.dark!=dark){
                        tapFooter("切换明暗模式","Toggle light/dark")
                        await("the visible footer can change the theme with a panel open"){model!!.state.value.dark==dark&&!model!!.state.value.busy}
                    }
                    android.os.SystemClock.sleep(220)
                    screenshot(shot.removeSuffix(".png")+if(dark)"-dark.png"else"-light.png")
                }
                if(model!!.state.value.dark!=previousDark){
                    tapFooter("切换明暗模式","Toggle light/dark")
                    await("theme preference is restored"){model!!.state.value.dark==previousDark&&!model!!.state.value.busy}
                }
            }
            tapFooter("批注与高亮","Notes and highlights")
            await("tapping the active footer action closes its panel"){node {it.paneTitle?.toString() in setOf("批注与高亮面板","Notes and highlights panel")}==null}
            tapFooter("目录与搜索","Contents and search")
            await("directory fold state survives a round trip through other panels"){label("展开章节","Expand chapter")!=null&&label("Chapter One")==null}
            await("the full-height drawer finishes its entrance before testing IME motion"){
                node {it.paneTitle?.toString() in setOf("目录与搜索","Contents and search")}?.let {Rect().also(it::getBoundsInScreen).top==0}==true
            }
            click(label("展开章节","Expand chapter"))
            await("parent expands its children"){label("收起章节","Collapse chapter")!=null}
            val paneBefore=Rect().also {node {it.paneTitle?.toString() in setOf("目录与搜索","Contents and search")}!!.getBoundsInScreen(it)}
            val headerBefore=Rect().also {label("目录与搜索栏","Navigation header")!!.getBoundsInScreen(it)}
            fun assertFixedDuringKeyboard(action:()->Unit){
                action()
                repeat(2){
                    val pane=node {it.paneTitle?.toString() in setOf("目录与搜索","Contents and search")}
                    val header=label("目录与搜索栏","Navigation header")
                    if(pane!=null&&header!=null){
                        assertEquals("The sheet frame stays fixed throughout IME motion",paneBefore,Rect().also(pane::getBoundsInScreen))
                        assertEquals("The search header is never pushed by the IME",headerBefore,Rect().also(header::getBoundsInScreen))
                    }
                    android.os.SystemClock.sleep(12)
                }
            }
            assertFixedDuringKeyboard {click(label("搜本书","Search book"))}
            await("search field takes focus"){node {it.className=="android.widget.EditText"}!=null}
            assertNull("Search replaces both tabs",label("搜本书","Search book"))
            assertNull(label("目录","Contents"))
            assertNotNull(label("取消","Cancel"))
            assertNull("Blank searches have no empty-state message",label("搜索本书正文","Search the book text"))
            val fieldBounds=Rect().also {node {it.className=="android.widget.EditText"}!!.getBoundsInScreen(it)}
            // Minimum touch bounds round independently of the 40dp visual pill.
            assertTrue("Typing happens in the original header row: $headerBefore / $fieldBounds",Rect(headerBefore).apply {inset(-2,-2)}.contains(fieldBounds))
            fun imeVisible()=WindowInsetsCompat.toWindowInsetsCompat(activity!!.window.decorView.rootWindowInsets).isVisible(WindowInsetsCompat.Type.ime())
            await("search opens the keyboard"){imeVisible()}
            await("keyboard overlays the fixed surface instead of moving it"){
                val pane=node {it.paneTitle?.toString() in setOf("目录与搜索","Contents and search")} ?: return@await false
                val bounds=Rect();pane.getBoundsInScreen(bounds)
                val root=activity!!.window.decorView
                val inset=WindowInsetsCompat.toWindowInsetsCompat(root.rootWindowInsets).getInsets(WindowInsetsCompat.Type.ime()).bottom
                bounds==paneBefore&&bounds.bottom>root.height-inset
            }
            instrumentation.runOnMainSync {reader(activity!!.window.decorView)!!.let {assertEquals("IME never repaginates the book",size,it.width to it.height)}}
            screenshot("navigation-search-keyboard.png")
            assertFixedDuringKeyboard {click(label("取消","Cancel"))}
            await("leaving search dismisses the keyboard"){!imeVisible()}
            await("cancel restores the original tabs"){label("搜本书","Search book")!=null&&label("目录","Contents")!=null}
            click(label("搜本书","Search book"))
            await("search can focus again"){imeVisible()}
            val args=Bundle().apply {putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE,"Independent")}
            assertTrue(node {it.className=="android.widget.EditText"}!!.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT,args))
            await("search action available"){clickable(label("搜索","Search"))?.isEnabled==true}
            click(label("搜索","Search"))
            await("search uses shared native index"){model!!.state.value.hits.isNotEmpty()}
            await("results shown in drawer"){node {it.text?.toString()?.contains("Independent")==true&&it.className!="android.widget.EditText"}!=null}
            click(node {it.text?.toString()?.contains("Independent")==true&&it.className!="android.widget.EditText"})
            await("result closes drawer"){label("目录与搜索栏","Navigation header")==null}
            await("navigation returns to unobstructed reading"){!model!!.state.value.chromeVisible&&label("开启翻译","Turn translation on")==null}
            android.os.SystemClock.sleep(220)
            var tapX=0f;var tapY=0f
            instrumentation.runOnMainSync {
                val native=reader(activity!!.window.decorView)!!;val at=IntArray(2);native.getLocationOnScreen(at)
                val active=model!!.state.value.frame!!.info.focusBounds.firstOrNull()
                val density=native.resources.displayMetrics.density
                tapX=at[0]+if(active!=null)(active.x+active.width/2)*density else native.width*.5f
                tapY=at[1]+if(active!=null)((active.y+active.height/2)*density).coerceIn(8f,native.height-8f)else native.height*.5f
            }
            val tapTime=android.os.SystemClock.uptimeMillis()
            android.view.MotionEvent.obtain(tapTime,tapTime,0,tapX,tapY,0).also {instrumentation.sendPointerSync(it);it.recycle()}
            android.view.MotionEvent.obtain(tapTime,tapTime+60,1,tapX,tapY,0).also {instrumentation.sendPointerSync(it);it.recycle()}
            await("the closed panel releases real book input and controls can reopen"){label("目录与搜索","Contents and search")!=null}
            instrumentation.runOnMainSync {model!!.readerMotion(true)}
            assertTrue("Programmatic motion alone does not change chrome",model!!.state.value.chromeVisible)
            instrumentation.runOnMainSync {model!!.readerMotion(false);reader(activity!!.window.decorView)!!.onReadingGesture?.invoke()}
            await("actual reading gestures hide the controls"){!model!!.state.value.chromeVisible&&label("目录与搜索","Contents and search")==null}
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

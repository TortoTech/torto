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
    private fun <T> onMain(block:()->T):T {
        var result:Result<T>?=null
        instrumentation.runOnMainSync {result=runCatching(block)}
        return result!!.getOrThrow()
    }

    private fun fixture(file:File,ambiguous:Boolean=false,identity:String="reader-navigation-ui"){
        val body="<h1 id='one'>Chapter One</h1><p>Independent paragraph for navigation and search. This is a second sentence.</p>"+(0..15).joinToString(""){"<p>Paragraph $it keeps source locations stable while controls show and hide. A second sentence remains in this paragraph.</p>"}+"<h2 id='two'>Chapter Two</h2><p>Another chapter in the same source.</p><h2 id='appendix'>Appendix</h2><p>End notes.</p>"
        val files=mapOf(
            "mimetype" to "application/epub+zip",
            "META-INF/container.xml" to """<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="package.opf"/></rootfiles></container>""",
            "package.opf" to """<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="uid"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="uid">$identity</dc:identifier><dc:title>Navigation UI fixture</dc:title><dc:language>en</dc:language></metadata><manifest><item id="body" href="body.xhtml" media-type="application/xhtml+xml"/><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/></manifest><spine><itemref idref="body"/></spine></package>""",
            "body.xhtml" to """<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Navigation</title></head><body>$body</body></html>""",
            "nav.xhtml" to """<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><head><title>Contents</title></head><body><nav epub:type="toc"><ol><li><a href="body.xhtml">Part One</a><ol><li><a href="body.xhtml#one">Chapter One</a></li><li><a href="body.xhtml#two">Chapter Two</a></li></ol></li><li><a href="body.xhtml#appendix">Appendix</a></li></ol></nav></body></html>"""
        )
        val entries=if(ambiguous)files+("nav.xhtml" to """<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><head><title>Contents</title></head><body><nav epub:type="toc"><ol><li><a href="body.xhtml">Chapter One</a></li><li><a href="body.xhtml">Chapter Two</a></li><li><a href="body.xhtml">Appendix</a></li></ol></nav></body></html>""")else files
        java.util.zip.ZipOutputStream(file.outputStream()).use {zip->entries.forEach {(name,text)->zip.putNextEntry(java.util.zip.ZipEntry(name));zip.write(text.toByteArray());zip.closeEntry()}}
    }
    private fun screenshot(name:String){instrumentation.uiAutomation.takeScreenshot()?.let {bitmap->File(instrumentation.targetContext.cacheDir,name).outputStream().use {bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG,100,it)};bitmap.recycle()}}
    private fun darkBackdrop():Boolean=instrumentation.uiAutomation.takeScreenshot()?.let {bitmap->
        val dark=android.graphics.Color.luminance(bitmap.getPixel(bitmap.width/2,bitmap.height*4/5))<.5f
        bitmap.recycle();dark
    } ?: false

    @Test fun completionPageRequiresExplicitMarkAndReturnsToExactBookPosition():Unit=runBlocking {
        val core=NativeRuntime.core(instrumentation.targetContext)
        val originalAi=core.aiOptions();val originalReading=core.readingOptions();val before=core.books().map {it.id}.toSet()
        val file=File(instrumentation.targetContext.cacheDir,"book-end-ui.epub");fixture(file,true,"completion-ui-${System.nanoTime()}")
        val book=core.importBook(file.path).first()
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        var activity:MainActivity?=null;var model:ReaderModel?=null;var originalFocus=false;var original:ScreenState?=null
        try {
            core.saveAiOptions(originalAi.copy(layoutEnabled=false));core.saveReadingOptions(originalReading.copy(translated=false))
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity;assertNotNull(activity)
            instrumentation.runOnMainSync {model=ViewModelProvider(activity!!)[ReaderModel::class.java]}
            await("model loads"){model!!.state.value.loaded&&!model!!.state.value.busy};originalFocus=model!!.state.value.focus;original=model!!.state.value
            instrumentation.runOnMainSync {model!!.open(book)}
            await("book opens"){model!!.state.value.frame!=null&&model!!.state.value.toc.size==3&&!model!!.state.value.busy}
            val last=model!!.state.value.toc.last().id
            for(focus in listOf(false,true)){
                instrumentation.runOnMainSync {model!!.focus(focus)}
                await("reading mode settles"){model!!.state.value.focus==focus&&!model!!.state.value.busy}
                instrumentation.runOnMainSync {model!!.navigateToc(last)}
                await("last chapter opens"){model!!.state.value.frame?.info?.activeTocId==last&&!model!!.state.value.busy}
                val view=onMain {reader(activity!!.window.decorView)!!}
                // Switching focus changes the immersive viewport and schedules
                // a 100ms resize. Start the gesture on its displayed final frame.
                var observed:ReaderFrame?=null;var stableSince=0L
                await("the new viewport is displayed and has settled"){
                    val state=model!!.state.value;val now=android.os.SystemClock.uptimeMillis()
                    val displayed=onMain {ReaderView::class.java.getDeclaredField("frame").apply {isAccessible=true}.get(view)===state.frame}
                    if(state.busy||!displayed){stableSince=0;false}
                    else {
                        if(observed!==state.frame||stableSince==0L){observed=state.frame;stableSince=now}
                        now-stableSince>=180
                    }
                }
                val calls=java.util.concurrent.atomic.AtomicInteger()
                instrumentation.runOnMainSync {
                    if(focus){val navigate=view.onUnitTurn!!;view.onUnitTurn={calls.incrementAndGet();navigate(it)}}
                    else {val navigate=view.onTurn!!;view.onTurn={calls.incrementAndGet();navigate(it)}}
                }
                fun swipe(forward:Boolean){instrumentation.runOnMainSync {
                    val start=android.os.SystemClock.uptimeMillis();val from=view.width*if(forward).8f else .2f;val to=view.width*if(forward).3f else .7f
                    for((action,x,time)in listOf(Triple(android.view.MotionEvent.ACTION_DOWN,from,start),Triple(android.view.MotionEvent.ACTION_MOVE,to,start+100),Triple(android.view.MotionEvent.ACTION_UP,to,start+300)))android.view.MotionEvent.obtain(start,time,action,x,view.height*.5f,0).also {view.onTouchEvent(it);it.recycle()}
                }}
                val frame=model!!.state.value.frame!!
                var finishedEvents:Int?=null
                var ordinal:UInt?=null
                repeat(2){index->
                    val completion=model!!.state.value.turnCompletion
                    swipe(true)
                    await("a forward book boundary opens the completion page"){model!!.state.value.turnCompletion>completion&&model!!.state.value.completionPage&&!model!!.state.value.readerMoving}
                    await("the reading statistics are ready"){model!!.state.value.completion!=null&&!model!!.state.value.completionLoading}
                    assertEquals(index+1,calls.get());assertEquals(last,model!!.state.value.frame!!.info.activeTocId)
                    val returned=model!!.state.value.frame!!
                    assertEquals(frame.info.section,returned.info.section);assertEquals(frame.info.page,returned.info.page)
                    assertEquals(frame.info.progress,returned.info.progress,0.0)
                    val detail=model!!.state.value.completion!!.statistics
                    if(!focus&&index==0){
                        assertNotEquals("Entering the end page must not mark the book", "finished",detail.status)
                        assertNotNull(label("标记读完","Mark finished"));assertNotNull(label("全书完","The End"))
                        screenshot("completion-unmarked.png")
                    }
                    if(focus&&index==0){
                        click(label("标记读完","Mark finished"))
                        await("only an explicit click marks the book finished"){model!!.state.value.completion!!.statistics.status=="finished"&&!model!!.state.value.completionLoading}
                        finishedEvents=core.bookStatistics(book.id).history.count {it.kind=="finished"}
                        ordinal=model!!.state.value.completion!!.finishedBooks
                        assertEquals(1,finishedEvents)
                        assertTrue(ordinal!!>0u)
                        await("finished heading is displayed"){label("已读完","Finished")!=null&&label("标记读完","Mark finished")==null}
                        screenshot("completion-marked.png")
                    }
                    if(focus&&index==1){
                        assertEquals("finished",detail.status)
                        assertEquals("Reopening does not add duplicate completion events",finishedEvents,core.bookStatistics(book.id).history.count {it.kind=="finished"})
                        assertEquals(ordinal,model!!.state.value.completion!!.finishedBooks)
                    }
                    if(focus&&index==1){
                        click(label("退出阅读","Exit reading"))
                        await("exit reading returns to the shelf"){model!!.state.value.active==null&&!model!!.state.value.busy}
                    }else{
                        // Exercise the visible completion page's rightward swipe,
                        // rather than issuing another native previous-page turn.
                        val start=android.os.SystemClock.uptimeMillis();val y=view.height*.4f
                        fun event(action:Int,x:Float){val time=android.os.SystemClock.uptimeMillis();android.view.MotionEvent.obtain(start,time,action,x,y,0).also {instrumentation.sendPointerSync(it);it.recycle()}}
                        event(android.view.MotionEvent.ACTION_DOWN,view.width*.2f)
                        for(step in 1..6){android.os.SystemClock.sleep(15);event(android.view.MotionEvent.ACTION_MOVE,view.width*(.2f+.1f*step))}
                        event(android.view.MotionEvent.ACTION_UP,view.width*.8f)
                        await("swiping back reveals the exact last page or section"){!model!!.state.value.completionPage}
                        assertEquals(last,model!!.state.value.frame!!.info.activeTocId)
                        assertEquals(frame.info.page,model!!.state.value.frame!!.info.page)
                        assertEquals("Synthetic-page back never turns native content",index+1,calls.get())
                    }
                }
                if(!focus){
                    val completion=model!!.state.value.turnCompletion
                    swipe(false)
                    await("native reverse paging still works after returning from the end page"){model!!.state.value.turnCompletion>completion&&!model!!.state.value.busy&&!model!!.state.value.readerMoving}
                    assertEquals(3,calls.get());assertNotEquals(last,model!!.state.value.frame!!.info.activeTocId)
                }
                assertNull(model!!.state.value.error)
            }
        }finally {
            model?.let {m->
                if(m.state.value.active!=null){
                    instrumentation.runOnMainSync {m.focus(originalFocus)};await("reading mode restores"){!m.state.value.busy&&m.state.value.focus==originalFocus}
                    instrumentation.runOnMainSync {m.back()};await("reader closes"){m.state.value.active==null&&!m.state.value.busy}
                }
            }
            original?.let {saved->
                instrumentation.targetContext.getSharedPreferences("reading",0).edit().putBoolean("focus",originalFocus).commit()
                instrumentation.runOnMainSync {model!!.configure(saved.fontSize,saved.unified,saved.dark)}
                await("reading preferences restore"){!model!!.state.value.busy}
            }
            core.saveAiOptions(originalAi);core.saveReadingOptions(originalReading)
            activity?.let {instrumentation.runOnMainSync {it.finish()}};instrumentation.removeMonitor(monitor)
            if(book.id !in before)core.removeBook(book.id);file.delete()
        }
    }

    @Test fun notesGroupByChapterAndSearchQuotesAndCommentText():Unit=runBlocking {
        val context=instrumentation.targetContext;val core=NativeRuntime.core(context)
        val originalAi=core.aiOptions();val originalReading=core.readingOptions();val before=core.books().map {it.id}.toSet()
        val file=File(context.cacheDir,"grouped-notes.epub");fixture(file,true,"grouped-notes-ui")
        val book=core.importBook(file.path).first()
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        var activity:MainActivity?=null;var model:ReaderModel?=null;var previous:ScreenState?=null
        try {
            core.saveAiOptions(originalAi.copy(layoutEnabled=false));core.saveReadingOptions(originalReading.copy(translated=false))
            val native=core.openBook(book.id,org.tortotech.torto.core.Viewport(360u,640u,2f))
            try {
                native.focusMode(false).close();native.selectionMode("paragraph")
                // Save in reverse chapter order to exercise source ordering.
                for((query,note) in listOf("Another chapter" to "A needle only in this comment", "Independent paragraph" to null)){
                    val hit=native.search(query).first()
                    native.goToSource(hit.source).use {page->
                        val rect=page.info().regions.first {it.text.contains(query)}.bounds.first()
                        assertNotNull(native.select(rect.x+2,rect.y+rect.height/2,rect.x+20,rect.y+rect.height/2))
                    }
                    native.saveAnnotation(note)
                }
                val position=native.page().use {it.info().progress}
                val notes=native.annotations()
                assertEquals(listOf("Chapter One","Chapter Two"),notes.map {it.chapterTitle})
                assertEquals(2,notes.map {it.chapterId}.distinct().size)
                assertEquals("Listing notes must not move the reader",position,native.page().use {it.info().progress},0.0)
            }finally{native.shutdown();native.close()}
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity;assertNotNull(activity)
            instrumentation.runOnMainSync {model=ViewModelProvider(activity!!)[ReaderModel::class.java]}
            await("model loads"){model!!.state.value.loaded&&!model!!.state.value.busy}
            previous=model!!.state.value
            instrumentation.runOnMainSync {model!!.interfaceSettings("light","en");model!!.open(book)}
            await("notes fixture opens"){model!!.state.value.active?.id==book.id&&model!!.state.value.frame!=null&&!model!!.state.value.busy}
            instrumentation.runOnMainSync {model!!.configure(model!!.state.value.fontSize,true,false)}
            await("light reading palette applies"){!model!!.state.value.dark&&!model!!.state.value.busy}
            instrumentation.runOnMainSync {if(!model!!.state.value.chromeVisible)model!!.toggleChrome()}
            await("notes footer is available"){label("Notes and highlights")!=null};click(label("Notes and highlights"))
            await("both chapters and two note types render"){label("Notes")!=null&&label("Chapter One")!=null&&label("Chapter Two")!=null&&label("Highlight")!=null&&label("Note")!=null}
            await("light note cards are actually drawn"){!darkBackdrop()}
            assertNull(label("Edit note"));assertNull(label("Delete annotation"))
            assertNotNull(label("2 notes"));screenshot("notes-grouped-light.png")
            val first=Rect().also {label("Chapter One")!!.getBoundsInScreen(it)}
            val second=Rect().also {label("Chapter Two")!!.getBoundsInScreen(it)}
            assertTrue("Chapter groups follow source order",first.top<second.top)
            instrumentation.runOnMainSync {model!!.configure(model!!.state.value.fontSize,true,true)}
            await("dark notes settle"){model!!.state.value.dark&&!model!!.state.value.busy&&darkBackdrop()}
            screenshot("notes-grouped-dark.png")
            click(label("Search notes"))
            await("note search uses an inline input"){node {it.className?.toString()=="android.widget.EditText"}!=null}
            assertNull(label("0 notes found"))
            val viewport=onMain {reader(activity!!.window.decorView)!!.let {it.width to it.height}}
            fun enter(text:String){
                val field=node {it.className?.toString()=="android.widget.EditText"}!!
                assertTrue(field.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT,Bundle().apply {putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE,text)}))
            }
            enter("iNdePenDent")
            await("search finds highlight quotes ignoring case"){label("1 note found")!=null&&label("Chapter One")!=null&&label("Chapter Two")==null}
            enter("needle")
            await("search also finds comment text"){label("1 note found")!=null&&label("Chapter Two")!=null&&label("Chapter One")==null}
            await("search opens the real keyboard"){onMain {androidx.core.view.ViewCompat.getRootWindowInsets(activity!!.window.decorView)?.isVisible(WindowInsetsCompat.Type.ime())==true}}
            assertEquals("Searching notes keeps native pagination stable",viewport,onMain {reader(activity!!.window.decorView)!!.let {it.width to it.height}})
            screenshot("notes-search-keyboard.png")
            click(label("Cancel"))
            await("cancel returns to the grouped list"){label("Notes")!=null&&label("Chapter One")!=null&&label("Chapter Two")!=null}
            click(label("Search notes"))
            await("search reopens"){node {it.className?.toString()=="android.widget.EditText"}!=null}
            enter("needle")
            await("comment result is available"){label("A needle only in this comment")!=null}
            click(label("A needle only in this comment"))
            await("result opens note view at its exact chapter"){label("Notes list")!=null&&label("Search notes")==null&&!model!!.state.value.busy&&model!!.state.value.frame?.info?.activeTocId==model!!.state.value.annotations.last().chapterId&&model!!.state.value.annotationPreviewId==model!!.state.value.annotations.last().id}
            assertNull("Ordinary floating actions are hidden in note view",label("Reading assistant"))
            assertTrue(clickable(label("Previous note"))!!.isEnabled)
            assertFalse("The last note has no next target",clickable(label("Next note"))!!.isEnabled)
            assertEquals("Note toolbars overlay the unchanged page",viewport,onMain {reader(activity!!.window.decorView)!!.let {it.width to it.height}})
            screenshot("notes-reading-preview.png")
            click(label("Back to notes"))
            await("the header menu returns to the notes panel"){label("Search notes")!=null&&label("Highlight")!=null}
            click(label("Highlight"))
            await("highlight cards enter the same note view"){label("Notes list")!=null&&!model!!.state.value.busy&&model!!.state.value.annotationPreviewId==model!!.state.value.annotations.first().id}
            assertFalse("The first note has no previous target",clickable(label("Previous note"))!!.isEnabled)
            click(label("Next note"))
            await("next visits the following note without losing the toolbars"){model!!.state.value.annotationPreviewId==model!!.state.value.annotations.last().id&&!model!!.state.value.busy&&label("Notes list")!=null}
            click(label("Previous note"))
            await("previous returns to the first highlight"){model!!.state.value.annotationPreviewId==model!!.state.value.annotations.first().id&&!model!!.state.value.busy}
            click(label("Notes list"))
            await("the bottom list action also returns to notes"){label("Search notes")!=null&&label("Highlight")!=null}
            click(label("Highlight"))
            await("preview reopens"){label("Exit note view")!=null&&!model!!.state.value.busy}
            click(label("Exit note view"))
            await("closing exits to ordinary immersive reading"){model!!.state.value.annotationPreviewId==null&&label("Notes list")==null}
            instrumentation.runOnMainSync {model!!.toggleChrome()}
            await("ordinary reading controls are available again"){label("Contents and search")!=null}
            assertEquals("Searching never mutates notes",2,model!!.state.value.annotations.size)
        }finally {
            model?.let {m->instrumentation.runOnMainSync {m.back()};await("reader closes"){m.state.value.active==null&&!m.state.value.busy}}
            core.saveAiOptions(originalAi);core.saveReadingOptions(originalReading)
            previous?.let {saved->instrumentation.runOnMainSync {model!!.interfaceSettings(saved.appTheme,saved.language)};await("interface restores"){!model!!.state.value.busy}}
            activity?.let {instrumentation.runOnMainSync {it.finish()}};instrumentation.removeMonitor(monitor)
            if(book.id !in before)core.removeBook(book.id);file.delete()
        }
    }

    @Test fun ambiguousContentsNavigateByIdentityAndKeepSectionOrder():Unit=runBlocking {
        val context=instrumentation.targetContext
        val core=NativeRuntime.core(context)
        val before=core.books().map {it.id}.toSet()
        val originalAi=core.aiOptions();val originalReading=core.readingOptions()
        val file=File(context.cacheDir,"ambiguous-contents.epub");fixture(file,true)
        val book=core.importBook(file.absolutePath).first()
        core.saveAiOptions(originalAi.copy(layoutEnabled=false));core.saveReadingOptions(originalReading.copy(translated=false))
        val reader=core.openBook(book.id,org.tortotech.torto.core.Viewport(360u,640u,2f))
        try {
            val rows=reader.toc();assertEquals(3,rows.size);assertEquals(1,rows.map {it.href}.distinct().size)
            for(focus in listOf(false,true)) {
                reader.focusMode(focus).close()
                var progress=-1.0
                for(row in rows) {
                    reader.navigateToc(row.id).use {page->val info=page.info();assertEquals(row.id,info.activeTocId);assertTrue(info.progress>=progress);progress=info.progress}
                }
                reader.navigateToc(rows[0].id).close()
                for(index in listOf(1,2))reader.turnReadingUnit(true).use {assertEquals(rows[index].id,it.info().activeTocId)}
                for(index in listOf(1,0))reader.turnReadingUnit(false).use {assertEquals(rows[index].id,it.info().activeTocId)}
            }
        }finally {
            reader.shutdown();reader.close()
            core.saveAiOptions(originalAi);core.saveReadingOptions(originalReading)
            if(book.id !in before)core.removeBook(book.id)
            file.delete()
        }
    }

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
                val density=context.resources.displayMetrics.density
                var frame=model!!.state.value.frame!!
                var observed:ReaderFrame?=null
                var stableSince=0L
                await("reference marker belongs to an actionable, unchanged frame") {
                    val snapshot=model!!.state.value
                    if(snapshot.busy){observed=null;return@await false}
                    val candidate=snapshot.frame ?: return@await false
                    if(observed!==candidate){observed=candidate;stableSince=android.os.SystemClock.uptimeMillis();return@await false}
                    // Insets animate on launch; wait for their debounced native
                    // resize before asserting that opening a note reuses pixels.
                    if(android.os.SystemClock.uptimeMillis()-stableSince<350)return@await false
                    val bitmap=candidate.bitmap
                    val height=minOf(bitmap.height,(180*density).toInt())
                    val pixels=IntArray(bitmap.width*height);bitmap.getPixels(pixels,0,bitmap.width,0,0,bitmap.width,height)
                    val accent=org.tortotech.torto.core.referenceColor(dark).toInt()
                    var left=bitmap.width;var top=height;var right=0;var bottom=0
                    pixels.forEachIndexed {index,pixel->if(pixel==accent){val x=index%bitmap.width;val y=index/bitmap.width;left=minOf(left,x);right=maxOf(right,x);top=minOf(top,y);bottom=maxOf(bottom,y)}}
                    if(left>right||top>bottom)return@await false
                    var dispatched=false
                    // A debounced viewport resize can begin while scanning pixels.
                    instrumentation.runOnMainSync {
                        if(!model!!.state.value.busy&&model!!.state.value.frame===candidate){
                            frame=candidate;model!!.tap((left+right)/2f/density,(top+bottom)/2f/density);dispatched=true
                        }
                    }
                    dispatched
                }
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
                Box(Modifier.fillMaxSize()){ReadingChrome(screen.value,blocked,null,{},{},{},{})}
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
            android.os.SystemClock.sleep(350)
            assertNull("AI processing has no header pause/stop entry",label("Stop processing"))
            assertEquals("AI controls must not shift chat",chat,bounds("Reading assistant"))
            instrumentation.runOnMainSync {screen.value=screen.value.copy(processing=false,enhancing=true)}
            android.os.SystemClock.sleep(350)
            assertNull("Optional generation has no header pause/stop entry",label("Stop processing"))
            instrumentation.runOnMainSync {screen.value=screen.value.copy(busy=true)}
            await("long operations still disable conflicting controls"){!button("Reading assistant").isEnabled}
            instrumentation.runOnMainSync {screen.value=screen.value.copy(busy=false,processing=false,enhancing=false)}
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
            fun immersiveBounds():Rect {
                val bounds=Rect()
                instrumentation.runOnMainSync {
                    val view=reader(activity!!.window.decorView)!!
                    val position=IntArray(2);view.getLocationInWindow(position)
                    bounds.set(position[0],position[1],position[0]+view.width,position[1]+view.height)
                }
                return bounds
            }
            await("Focus content extends through both system-bar and cutout bands"){
                val bounds=immersiveBounds()
                bounds.top==0&&bounds.bottom==activity!!.window.decorView.height&&!model!!.state.value.busy
            }
            val fullScreen=immersiveBounds()
            // Wait for the debounced native resize before comparing retained rasters.
            android.os.SystemClock.sleep(250)
            await("the full-screen native resize finishes"){!model!!.state.value.busy}
            val active=model!!.state.value.frame!!.info.focusBounds.first()
            val raster=model!!.state.value.frame!!.rasterId
            instrumentation.runOnMainSync {model!!.tap(active.x+2f,active.y+active.height/2)}
            await("tapping the active block reveals chrome"){model!!.state.value.chromeVisible&&!model!!.state.value.busy}
            await("status bar returns with the header"){statusVisible()}
            assertEquals("Showing controls must not reserve top or bottom bands",fullScreen,immersiveBounds())
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
            assertEquals("Hiding controls keeps the same immersive content plane",fullScreen,immersiveBounds())
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
            await("focus settings are shown"){label("专注模式","Focus mode")!=null}
            assertNull("Android always dims surroundings without a style choice",label("专注样式","Focus style"))
            assertFalse("Android does not highlight the focused block",model!!.state.value.frame!!.info.focusHighlight)
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

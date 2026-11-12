package org.tortotech.torto

import android.graphics.Bitmap
import androidx.activity.compose.setContent
import androidx.compose.animation.core.getVelocityFromNanos
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import kotlin.math.abs
import kotlinx.coroutines.runBlocking
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.tortotech.torto.core.*

@RunWith(AndroidJUnit4::class)
class ReadingIntegrationTest {
    @Test fun advancedFontsFocusSelectionAndProvidersRoundtrip():Unit=runBlocking {
        val context=InstrumentationRegistry.getInstrumentation().targetContext
        val core=MobileCore(File(context.filesDir,"core").absolutePath)
        val original=core.readingOptions()
        try {
            val reading=original.copy(fontWeight=575u,sentenceSplit=true,translated=false)
            core.saveReadingOptions(reading)
            assertEquals(575u.toUShort(),core.readingOptions().fontWeight)
            assertTrue(core.fontFamilies().monospace.contains("monospace"))
            checkAdvancedFocus(core,reading)
            checkProviderSettings(core)
            core.saveReadingOptions(reading.copy(translated=true));assertTrue(core.readingOptions().translated)
        }finally{core.saveReadingOptions(original);core.close()}
    }
    @Test fun nativeNotePagesUseReadingTypographyAndSelectionWithoutMovingBook():Unit=runBlocking {
        val instrumentation=InstrumentationRegistry.getInstrumentation()
        val context=instrumentation.targetContext
        val core=MobileCore(File(context.filesDir,"core").absolutePath)
        val before=core.books().map {it.id}.toSet()
        val fixture=File(context.cacheDir,"native-notes.epub")
        instrumentation.context.assets.open("reader-interactions.epub").use {input->fixture.outputStream().use {input.copyTo(it)}}
        val book=core.importBook(fixture.absolutePath).first {it.title=="阅读交互测试"}
        val reader=core.openBook(book.id,Viewport(360u,640u,2f))
        try {
            reader.configure(24f,true,false).close()
            val progress=reader.page().use {it.info().progress}
            val markup="**A reading note** with \\(x^2\\) and a [source](https://example.com)\n\n"+(0..25).joinToString("\n\n"){"Paragraph $it: long notes keep the reader typography and remain selectable."}
            val viewport=Viewport(300u,240u,2f)
            val first=reader.notePage(markup,1uL,"","",0f,0u,viewport,0xff000000u,0xfff8f5eeu)
            val id=first.id
            val total=first.total
            first.page.use {page->
                assertTrue(total>2u)
                assertTrue(page.info().regions.any {it.text.contains("reading note")})
                val bitmap=Bitmap.createBitmap(page.width().toInt(),page.height().toInt(),Bitmap.Config.ARGB_8888)
                assertTrue(NativePixels.copyPage(page.id().toLong(),bitmap));bitmap.recycle()
                val rect=page.info().regions.flatMap {it.bounds}.first()
                val selected=reader.documentSelect(id,0u,rect.x+2,rect.y+rect.height/2,rect.x+80,rect.y+rect.height/2,null)
                assertNotNull(selected);assertTrue(selected!!.text.isNotEmpty());assertTrue(selected.bounds.isNotEmpty())
            }
            val tail=StringBuilder()
            for(index in maxOf(0,total.toInt()-2) until total.toInt()){
                tail.append(reader.notePage(markup,1uL,"","",0f,index.toUInt(),viewport,0xff000000u,0xfff8f5eeu).page.use {page->
                    assertTrue(page.info().regions.isNotEmpty());page.info().regions.joinToString(""){it.text}
                })
            }
            assertTrue("The complete final note paragraph remains reachable",tail.contains("25:")&&tail.contains("selectable."))
            assertEquals(progress,reader.page().use {it.info().progress},0.000001)
            reader.configure(34f,true,false).close()
            val larger=reader.notePage(markup,1uL,"","",0f,0u,viewport,0xff000000u,0xfff8f5eeu)
            assertTrue("Notes must inherit reader font size",larger.total>total);larger.page.close()
            reader.closeDocument(id)
            assertTrue(runCatching {reader.referencePage(id,0u,viewport,0xff000000u,0xfff8f5eeu)}.isFailure)
        }finally {reader.shutdown();reader.close();if(book.id !in before)core.removeBook(book.id);core.close();fixture.delete()}
    }
    private suspend fun checkProviderSettings(core:MobileCore){
        val original=core.aiOptions()
        var providerId:String?=null
        try {
            core.saveProvider(Provider("","custom","Device test provider","http://127.0.0.1:1/v1",listOf("test-model"),false),"test-only",false)
            val provider=core.providers().single {it.name=="Device test provider"};providerId=provider.id
            assertTrue(provider.keySaved)
            val credentialFile=File(InstrumentationRegistry.getInstrumentation().targetContext.filesDir,"core/config/credentials.json")
            assertTrue("All hosts must use the shared plaintext store",credentialFile.readText().contains("test-only"))
            core.saveProvider(provider.copy(name="Device test renamed"),null,false)
            assertTrue(core.providers().single {it.id==provider.id}.keySaved)
            val ai=original.copy(layoutEnabled=false,layoutProvider=provider.id,layoutModel="test-model",layoutReasoning="none",translationProvider=provider.id,translationModel="test-model",translationReasoning="low",chatProvider=provider.id,chatModel="test-model",showOriginal=true,translateToc=true,expertTranslation=true)
            core.saveAiOptions(ai);assertEquals(ai,core.aiOptions())
        }finally{core.saveAiOptions(original);providerId?.let {core.deleteProvider(it)}}
    }
    private suspend fun checkAdvancedFocus(core:MobileCore,reading:ReadingOptions){
        val context=InstrumentationRegistry.getInstrumentation().targetContext
        val fixture=File(context.cacheDir,"advanced-reading.fb2")
        val paragraphs=(0..60).joinToString(""){ "<p>Paragraph $it: first sentence. A second sentence about reading and source selection.</p>" }
        fixture.writeText("""<?xml version="1.0" encoding="UTF-8"?><FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0"><description><title-info><book-title>字体与滚动专注测试</book-title><lang>en</lang></title-info></description><body><section><title><p>Settings</p></title>$paragraphs</section></body></FictionBook>""")
        val book=core.importBook(fixture.absolutePath).first {it.title=="字体与滚动专注测试"}
        val reader=core.openBook(book.id,Viewport(360u,640u,2f))
        try {
            ChatRequest().use {request->
                val idle=reader.enhancePageCancellable(request)
                assertNull(idle.page);assertFalse(idle.pending);assertFalse(idle.changed)
                request.cancel()
                val cancelled=runCatching {reader.enhancePageCancellable(request)}.exceptionOrNull()
                assertNotNull(cancelled);assertTrue(cancelled!!.message.orEmpty().contains("已停止"))
            }
            reader.goToSource(reader.search("Paragraph 0:").single().source).close()
            reader.focusMode(true).close()
            val page=reader.scrollBy(450f)
            val info=try{page.info()}finally{page.close()}
            assertTrue(info.isScroll);assertFalse(info.focusHighlight);assertEquals(450.0,info.scrollDistance,0.1)
            checkNativeHandles(reader,info)
            checkViewGestures(info)
            reader.applyReadingOptions(reading.copy(fontWeight=650u)).use {assertFalse(it.info().focusHighlight)}
            reader.focusMode(false).use {assertFalse(it.info().isScroll)}
        }finally{reader.shutdown();reader.close();fixture.delete()}
    }
    private suspend fun checkNativeHandles(reader:MobileReader,info:PageInfo){
        val visible=info.regions.filter {it.bounds.any {b->b.y>40f&&b.y+b.height<580f}}
        assertTrue(visible.size>=2)
        val a=visible[0].bounds.first();val b=visible[1].bounds.first()
        val selected=reader.select(a.x+2,a.y+a.height/2,a.x+25,a.y+a.height/2)!!
        reader.beginSelectionHandle(false)
        val extended=reader.extendSelection(b.x+40,b.y+b.height/2)!!
        assertTrue(extended.text.length>selected.text.length)
        reader.scrollBy(50f).close();assertEquals(extended.text,reader.selection()!!.text)
        reader.clearSelection()
    }
    private fun checkViewGestures(info:PageInfo){
        val instrumentation=InstrumentationRegistry.getInstrumentation()
        instrumentation.runOnMainSync {
            val context=instrumentation.targetContext
            val bitmap=Bitmap.createBitmap(720,1280,Bitmap.Config.ARGB_8888)
            val view=ReaderView(context);view.layout(0,0,720,1280)
            view.present(ReaderFrame(bitmap,info.copy(scrollDistance=0.0)),SelectedText("selected",listOf(Rect(30f,50f,80f,20f)),false))
            var handle:Boolean?=null;var moved=false;var ended=false;var scrolled=0f;var turns=0;var gestures=0
            view.onReadingGesture={gestures++};view.onHandleStart={handle=it};view.onHandleMove={_,_->moved=true};view.onHandleEnd={ended=true};view.onScroll={scrolled+=it};view.onTurn={turns++}
            val density=context.resources.displayMetrics.density;val start=android.os.SystemClock.uptimeMillis()
            fun event(action:Int,x:Float,y:Float,time:Long){val e=android.view.MotionEvent.obtain(start,time,action,x*density,y*density,0);view.onTouchEvent(e);e.recycle()}
            event(0,110f,76f,start);event(2,135f,90f,start+30);event(1,135f,90f,start+60)
            assertEquals(false,handle);assertTrue(moved&&ended)
            view.present(ReaderFrame(bitmap,info.copy(scrollDistance=0.0)),null)
            event(0,160f,180f,start+100);event(2,160f,170f,start+300);event(1,160f,170f,start+500)
            assertEquals("Short drags must return to the current block",0,turns)
            event(0,160f,180f,start+600);event(2,160f,100f,start+640);event(1,160f,100f,start+680)
            assertEquals("A swipe advances exactly one block",1,turns)
            assertEquals("Touch moves must not trigger native rasterization",0f,scrolled,0f)
            assertEquals("Each reading drag reports once; selection-handle drags do not",2,gestures)
            var unitTurns=0
            view.onUnitTurn={assertTrue(it);unitTurns++}
            event(0,260f,180f,start+800);event(2,90f,184f,start+840)
            assertEquals("Dragging horizontally does not commit before release",0,unitTurns)
            event(3,90f,184f,start+880)
            assertEquals(0,unitTurns)
            event(0,260f,180f,start+900);event(2,90f,184f,start+940);event(1,90f,184f,start+980)
            assertEquals("A pending block reply serializes the horizontal turn",0,unitTurns)
            view.present(ReaderFrame(bitmap,info.copy(scrollDistance=0.0)),null)
            assertEquals("Focus horizontal release changes semantic units, not focus blocks",1,unitTurns)
            assertEquals(1,turns)
            view.onWindowFocusChanged(false);bitmap.recycle()
        }
    }
    @Test fun focusSettlingPreservesVelocityAndSoftensBlockEdges(){
        for(velocity in listOf(0f,500f,8000f)){
            val curve=focusSettle(180f,velocity)
            assertTrue(curve.durationNanos in 1..800_000_000L)
            assertEquals(0f,curve.getValueFromNanos(0),.001f)
            assertEquals(velocity.coerceAtMost(5400f),curve.getVelocityFromNanos(0),.1f)
            var previous=0f
            for(t in 0..80){val value=curve.getValueFromNanos(curve.durationNanos*t/80)
                assertTrue("Settling must stay monotone without bouncing",value>=previous-.01f&&value<=180.01f);previous=value}
            assertEquals(180f,curve.getValueFromNanos(curve.durationNanos),.001f)
            assertEquals(0f,curve.getVelocityFromNanos(curve.durationNanos),.001f)
        }
        val near=focusDragPosition(101.0,0.0,100.0,640f)
        assertTrue(near>100.9&&near<101.0)
        assertTrue(focusDragPosition(1000.0,0.0,100.0,640f)<164.01)
        assertEquals(50.0,focusDragPosition(50.0,0.0,100.0,640f),.0001)
    }
    @Test fun shortFocusStopsDoNotTravelPastSeveralParagraphsOrSkipOnReversal(){
        // A 32dp stop used to allow a further 64dp of travel, then spring back.
        for(start in listOf(0.0,1200.0)){
            assertTrue(focusDragPosition(start+300,start-32,start+32,640f,start)<=start+40.01)
            assertTrue(focusDragPosition(start-300,start-32,start+32,640f,start)>=start-40.01)
            assertEquals(start+20,focusDragPosition(start+20,start-32,start+32,640f,start),.001)
        }
        assertEquals(true,focusDragDestination(18f,0f,-32f,32f))
        assertEquals(false,focusDragDestination(-18f,0f,-32f,32f))
        assertNull(focusDragDestination(10f,0f,-32f,32f))
        assertNull("A reversing flick returns to the same block",focusDragDestination(60f,-800f,-32f,32f))
        assertNull(focusDragDestination(-60f,800f,-32f,32f))
        assertEquals(true,focusDragDestination(8f,800f,-32f,32f))
        assertNull("Long paragraphs retain the existing confirmation distance",focusDragDestination(18f,0f,-200f,200f))
        assertEquals(true,focusDragDestination(36f,0f,-200f,200f))
        assertEquals("A short unit edge keeps the same confirmation scale",true,focusDragDestination(18f,0f,null,null))
        assertNull("Touch noise still cannot turn a section",focusDragDestination(6f,0f,null,null))
        assertEquals(true,focusDragDestination(18f,0f,-32f,null))
        for(distance in listOf(4f,-4f,32f,-32f)){
            val curve=focusSettle(distance,0f)
            for(t in 0..40)assertTrue("Short settling cannot cross the chosen stop",abs(curve.getValueFromNanos(curve.durationNanos*t/40))<=abs(distance)+.01f)
        }
        val instrumentation=InstrumentationRegistry.getInstrumentation()
        instrumentation.runOnMainSync {
            val context=instrumentation.targetContext;val density=context.resources.displayMetrics.density
            val width=(360*density).toInt();val height=(640*density).toInt();val pad=160f
            val bitmap=Bitmap.createBitmap(width,(960*density).toInt(),Bitmap.Config.ARGB_8888)
            val output=Bitmap.createBitmap(width,height,Bitmap.Config.ARGB_8888)
            val view=ReaderView(context);view.layout(0,0,width,height)
            var info=PageInfo(section=0u,sectionCount=1u,page=0u,pageCount=1u,progress=0.0,regions=emptyList(),marks=emptyList(),focusBounds=emptyList(),
                focusFadeColor=0u,focusHighlight=false,isScroll=true,scrollDistance=0.0,scrollContext=9200uL,rasterTop=-pad,rasterHeight=960f,
                previousFocus=FocusTarget(-32f,emptyList()),nextFocus=FocusTarget(32f,emptyList()),controls=emptyList(),activeTocId=null,activeTocPath=emptyList(),focusIndex=1u,focusCount=3u,canGoBack=false)
            var turns=0
            try {
                bitmap.eraseColor(android.graphics.Color.WHITE)
                android.graphics.Canvas(bitmap).drawRect(0f,480*density,width.toFloat(),484*density,android.graphics.Paint().apply {color=android.graphics.Color.BLACK})
                view.present(ReaderFrame(bitmap,info,92uL),null)
                view.onScroll={fail("Dragging a short paragraph never rasterizes")}
                view.onTurn={forward->
                    assertEquals(turns==0,forward);turns++
                    val offset=if(forward)32.0 else 0.0
                    info=info.copy(scrollDistance=offset,rasterTop=-pad-offset.toFloat())
                    view.present(ReaderFrame(bitmap,info,92uL),null)
                }
                var clock=android.os.SystemClock.uptimeMillis()
                fun event(action:Int,y:Float,time:Long){android.view.MotionEvent.obtain(clock,time,action,180*density,y*density,0).also {view.onTouchEvent(it);it.recycle()}}
                event(0,280f,clock);event(2,120f,clock+80)
                view.draw(android.graphics.Canvas(output))
                val stripe=(0 until height).first {output.getPixel(width/2,it)==android.graphics.Color.BLACK}/density
                assertTrue("Dragging cannot carry the visible text two short blocks past its target",320-stripe<=40.5f)
                event(3,120f,clock+200);assertEquals(0,turns)
                clock+=500
                event(0,280f,clock);event(2,250f,clock+120);event(1,250f,clock+300)
                assertEquals("A slow short swipe can confirm one short block",1,turns)
                clock+=500
                event(0,250f,clock);event(2,280f,clock+120);event(1,280f,clock+300)
                assertEquals("The short stop is reversible",2,turns)
                clock+=500
                event(0,280f,clock);event(2,268f,clock+120);event(1,268f,clock+300)
                assertEquals("Small deliberate motion still cancels",2,turns)
            }finally {view.onWindowFocusChanged(false);bitmap.recycle();output.recycle()}
        }
    }
    @Test(timeout=20000) fun focusDragUsesRetainedPixelsSnapsOnceAndStopsDrawing() {
        val instrumentation=InstrumentationRegistry.getInstrumentation()
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        var activity:MainActivity?=null
        var bitmap:Bitmap?=null
        val idle=java.util.concurrent.CountDownLatch(1)
        val moving=java.util.concurrent.atomic.AtomicBoolean()
        val turns=java.util.concurrent.atomic.AtomicInteger()
        val nativeScrolls=java.util.concurrent.atomic.AtomicInteger()
        val draws=java.util.concurrent.atomic.AtomicInteger()
        lateinit var view:ReaderView
        lateinit var info:PageInfo
        try {
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity
            assertNotNull(activity)
            instrumentation.runOnMainSync {
                view=ReaderView(activity!!)
                activity!!.setContentView(view)
            }
            instrumentation.waitForIdleSync()
            android.os.SystemClock.sleep(300)
            instrumentation.runOnMainSync {
                assertTrue("Motion needs a real attached window",view.isShown&&view.hasWindowFocus())
                val density=view.resources.displayMetrics.density
                val height=view.height/density
                val pad=160f
                bitmap=Bitmap.createBitmap(view.width,view.height+(2*pad*density).toInt(),Bitmap.Config.ARGB_8888).apply {eraseColor(android.graphics.Color.rgb(40,80,120))}
                info=PageInfo(section=0u,sectionCount=1u,page=0u,pageCount=1u,progress=0.0,
                    regions=emptyList(),marks=emptyList(),focusBounds=listOf(Rect(20f,200f,200f,60f)),
                    focusFadeColor=0x8CFFFFFFu,focusHighlight=false,isScroll=true,scrollDistance=0.0,
                    scrollContext=9001uL,controls=emptyList(),activeTocId=null,activeTocPath=emptyList(),rasterTop=-pad,rasterHeight=height+2*pad,
                    previousFocus=FocusTarget(-120f,listOf(Rect(20f,80f,200f,60f))),
                    nextFocus=FocusTarget(120f,listOf(Rect(20f,320f,200f,60f))),focusIndex=1u,focusCount=3u,canGoBack=false)
                view.present(ReaderFrame(bitmap!!,info,77uL),null)
                view.onMotion={active->moving.set(active);if(!active)idle.countDown()}
                view.onScroll={nativeScrolls.incrementAndGet()}
                view.onTurn={forward->
                    assertTrue(forward);turns.incrementAndGet()
                    info=info.copy(scrollDistance=120.0,rasterTop=-pad-120f,focusBounds=listOf(Rect(20f,200f,200f,60f)))
                    view.present(ReaderFrame(bitmap!!,info,77uL),null)
                }
                val start=android.os.SystemClock.uptimeMillis()
                fun event(action:Int,y:Float,t:Long){android.view.MotionEvent.obtain(start,t,action,180f*density,y*density,0).also {view.onTouchEvent(it);it.recycle()}}
                event(0,280f,start);event(2,210f,start+40)
                assertTrue(moving.get());assertEquals(0,turns.get());assertEquals(0,nativeScrolls.get())
                val output=Bitmap.createBitmap(view.width,view.height,Bitmap.Config.ARGB_8888)
                try {
                    view.draw(android.graphics.Canvas(output))
                    assertNotEquals("Dragging must reveal cached content at the top edge",android.graphics.Color.WHITE,output.getPixel(2,2))
                    assertNotEquals("Dragging must reveal cached content at the bottom edge",android.graphics.Color.WHITE,output.getPixel(2,view.height-3))
                }finally{output.recycle()}
                val clippedOutput=Bitmap.createBitmap(view.width,view.height+64,Bitmap.Config.ARGB_8888)
                try {
                    clippedOutput.eraseColor(android.graphics.Color.GREEN)
                    val canvas=android.graphics.Canvas(clippedOutput);canvas.translate(0f,32f)
                    view.draw(canvas)
                    assertEquals("Overscan must not paint over the header",android.graphics.Color.GREEN,clippedOutput.getPixel(2,8))
                    assertEquals("Overscan must not paint over the footer",android.graphics.Color.GREEN,clippedOutput.getPixel(2,view.height+48))
                }finally{clippedOutput.recycle()}
                event(1,210f,start+80)
            }
            assertTrue("Focus motion must settle",idle.await(2,java.util.concurrent.TimeUnit.SECONDS))
            assertFalse(moving.get());assertEquals(1,turns.get());assertEquals(0,nativeScrolls.get())
            // There must be no redraw timer left behind by the settled motion.
            android.os.SystemClock.sleep(200)
            instrumentation.runOnMainSync {view.viewTreeObserver.addOnDrawListener {draws.incrementAndGet()}}
            instrumentation.waitForIdleSync();draws.set(0)
            android.os.SystemClock.sleep(250)
            assertEquals("Settled focus must stop repainting",0,draws.get())
            instrumentation.runOnMainSync {
                val density=view.resources.displayMetrics.density;val start=android.os.SystemClock.uptimeMillis()
                fun event(action:Int,y:Float,t:Long){android.view.MotionEvent.obtain(start,t,action,180f*density,y*density,0).also {view.onTouchEvent(it);it.recycle()}}
                event(0,280f,start);event(2,210f,start+40);event(3,210f,start+80)
            }
            android.os.SystemClock.sleep(450);instrumentation.waitForIdleSync()
            assertFalse(moving.get());assertEquals("Cancelled gestures must not advance",1,turns.get())
            instrumentation.runOnMainSync {
                // An edge has no neighbouring focus block in the retained strip.
                info=info.copy(previousFocus=null,nextFocus=null,focusCount=1u,focusIndex=0u)
                view.present(ReaderFrame(bitmap!!,info,77uL),null)
                view.onTurn={forward->
                    assertTrue(forward);turns.incrementAndGet()
                    info=info.copy(scrollContext=9002uL,scrollDistance=0.0)
                    view.present(ReaderFrame(bitmap!!,info,78uL),null)
                }
                val density=view.resources.displayMetrics.density;val start=android.os.SystemClock.uptimeMillis()
                fun event(action:Int,y:Float,t:Long){android.view.MotionEvent.obtain(start,t,action,180f*density,y*density,0).also {view.onTouchEvent(it);it.recycle()}}
                event(0,280f,start);event(2,200f,start+40)
                assertEquals("Dragging at an edge cannot enter the next unit",1,turns.get())
                event(3,200f,start+80)
                assertEquals("Cancelling an edge drag keeps the current unit",1,turns.get())
                event(0,280f,start+100);event(2,200f,start+140);event(1,200f,start+180)
                assertEquals("Release starts the outgoing animation before requesting the unit",1,turns.get())
                assertEquals("Edge dragging never requests scroll/raster work",0,nativeScrolls.get())
            }
            val edgeDeadline=android.os.SystemClock.uptimeMillis()+2000
            while(turns.get()!=2||moving.get()){
                assertTrue("The unit turn completes after its outgoing animation",android.os.SystemClock.uptimeMillis()<edgeDeadline)
                android.os.SystemClock.sleep(10)
            }
            val queuedTurns=java.util.concurrent.atomic.AtomicInteger()
            instrumentation.runOnMainSync {
                info=info.copy(scrollContext=9100uL,scrollDistance=0.0,rasterTop=-160f,
                    previousFocus=FocusTarget(-60f,emptyList()),nextFocus=FocusTarget(60f,emptyList()),focusCount=3u)
                view.present(ReaderFrame(bitmap!!,info,80uL),null)
                // Hold the native reply across a second complete pointer sequence.
                view.onTurn={assertTrue(it);queuedTurns.incrementAndGet()}
                val density=view.resources.displayMetrics.density;val start=android.os.SystemClock.uptimeMillis()
                fun event(action:Int,y:Float,t:Long){android.view.MotionEvent.obtain(start,t,action,180f*density,y*density,0).also {view.onTouchEvent(it);it.recycle()}}
                event(0,280f,start);event(2,200f,start+40);event(1,200f,start+80)
                assertEquals(1,queuedTurns.get())
                event(0,280f,start+100);event(2,200f,start+140);event(1,200f,start+180)
                assertEquals("A pending native reply serializes the next committed gesture",1,queuedTurns.get())
                info=info.copy(scrollDistance=60.0,rasterTop=-220f)
                view.present(ReaderFrame(bitmap!!,info,80uL),null)
                assertEquals("The buffered swipe is applied exactly once after confirmation",2,queuedTurns.get())
                info=info.copy(scrollDistance=120.0,rasterTop=-280f)
                view.present(ReaderFrame(bitmap!!,info,80uL),null)
            }
            android.os.SystemClock.sleep(450);instrumentation.waitForIdleSync()
            assertFalse("Rapid consecutive swipes settle without leaving an input barrier",moving.get())
        }finally {
            activity?.let {host->instrumentation.runOnMainSync {host.finish()}}
            instrumentation.removeMonitor(monitor)
            // The view can retain the bitmap until its final detached frame.
        }
    }

    @Test(timeout=20000) fun assistantFormatsWhileTokensContinue() {
        val instrumentation=InstrumentationRegistry.getInstrumentation()
        val text=androidx.compose.runtime.mutableStateOf("**持续回复**")
        var formatted=false
        fun inspect(view:android.view.View) {
            if(view is android.widget.TextView&&view.text.toString().contains("持续回复")){
                val spans=(view.text as? android.text.Spanned)?.getSpans(0,view.text.length,android.text.style.StyleSpan::class.java).orEmpty()
                if(spans.any {it.style==android.graphics.Typeface.BOLD})formatted=true
            }
            if(view is android.view.ViewGroup)for(i in 0 until view.childCount)inspect(view.getChildAt(i))
        }
        // Launch our isolated preview directly. Some OEMs block the test-core
        // bootstrap activity's background launch/broadcast handoff.
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        var activity:MainActivity?=null
        try {
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity
            assertNotNull("Preview activity must start within five seconds",activity)
            val host=activity!!
            instrumentation.runOnMainSync {host.setContent {
                androidx.compose.material3.MaterialTheme {
                    val cache=androidx.compose.runtime.remember {AssistantAssetCache()}
                    AssistantRichText(text.value,cache,true){}
                }
            }}
            // Keep producing tokens faster than the formatter's cadence. A
            // trailing debounce would never produce styled text during this run.
            repeat(28){index->
                instrumentation.runOnMainSync {text.value="**持续回复** 内容 "+"续".repeat(index);inspect(host.window.decorView)}
                android.os.SystemClock.sleep(25)
            }
            assertTrue("Markdown must be formatted before a continuous stream ends",formatted)
        }finally {
            activity?.let {host->instrumentation.runOnMainSync {host.finish()}}
            instrumentation.removeMonitor(monitor)
        }
    }
    @Test fun assistantImagesHistoryAndCitationsRoundtrip()=runBlocking {
        val context=InstrumentationRegistry.getInstrumentation().targetContext
        val fixture=File(context.cacheDir,"assistant-fixture.fb2")
        val paragraphs=(0..25).joinToString(""){ "<p>段落 $it：这是用于核对阅读助手引用位置的测试内容。</p>" }
        fixture.writeText("""<?xml version="1.0" encoding="UTF-8"?><FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0"><description><title-info><book-title>阅读助手测试</book-title><lang>zh</lang></title-info></description><body><section><title><p>阅读助手</p></title>$paragraphs</section></body></FictionBook>""")
        val imageFile=File(context.cacheDir,"assistant-attachment.png")
        val bitmap=Bitmap.createBitmap(160,100,Bitmap.Config.ARGB_8888).apply {eraseColor(android.graphics.Color.rgb(30,140,100))}
        imageFile.outputStream().use {bitmap.compress(Bitmap.CompressFormat.PNG,100,it)};bitmap.recycle()
        val root=File(context.filesDir,"core")
        MobileCore(root.absolutePath).use {core->
            val book=core.importBook(fixture.absolutePath).first {it.title=="阅读助手测试"}
            val reader=core.openBook(book.id,Viewport(360u,640u,2f))
            val attachment:ChatAttachment
            val citation:String
            try {
                attachment=reader.addChatImage(imageFile.absolutePath,"设备附件测试.png")
                reader.chatImage(attachment.id).use {assertEquals(160u,it.width());assertEquals(100u,it.height())}
                val source=org.json.JSONObject(reader.search("段落 12：").single().source).getJSONObject("start")
                citation="link://j/0/"+android.net.Uri.encode(source.getString("node"))
                reader.followChatCitation(citation).use {assertTrue(it.info().regions.any {r->r.text.contains("段落 12：")});assertTrue(it.info().canGoBack)}
                reader.goBack().use {assertFalse(it.info().canGoBack)}
                try{reader.chatImage("../../secret").close();fail("Invalid image identifier must fail")}catch(_:CoreException){}
            }finally{reader.shutdown();reader.close()}
            val markdown="## 阅读助手\n\n**重点内容**、*强调*和 `代码`，行内公式 \$x^2\$\n\n> 这是一段引用\n\n- 第一项\n- 第二项\n\n|项目|数值|\n|---|---|\n|甲|10|\n|乙|20|\n\n\$\$\\frac{1}{2}\$\$\n\n[查看第 12 段]("+citation+")"
            val parsed=decodeAssistantMarkdown(chatMarkdown(markdown))
            assertTrue(parsed.any {it.kind=="table"&&it.rows.size==3})
            assertTrue(parsed.flatMap {it.runs}.any {it.math})
            assertTrue(parsed.flatMap {it.runs}.any {it.link==citation})
            val messages=org.json.JSONArray().put(org.json.JSONObject().put("user",true).put("content","请结合图片和书中内容解释").put("images",org.json.JSONArray().put(org.json.JSONObject().put("id",attachment.id).put("label",attachment.label)))).put(org.json.JSONObject().put("user",false).put("content",markdown))
            val history=File(root,"chat/${book.id}.json");history.parentFile!!.mkdirs();history.writeText(messages.toString())
            core.openBook(book.id,Viewport(360u,640u,2f)).use {reopened->
                try {
                    assertTrue("A new reading session never restores a transcript file",reopened.chatHistory().isEmpty())
                    try{reopened.chatImage(attachment.id).close();fail("Closed reading sessions release their image cache")}catch(_:CoreException){}
                }finally{reopened.shutdown()}
            }
            history.delete()
        }
        fixture.delete();imageFile.delete()
        Unit
    }
    @Test fun focusSurroundingsFadeToPaperInBothThemes()=runBlocking {
        val instrumentation=InstrumentationRegistry.getInstrumentation()
        val context=instrumentation.targetContext
        val fixture=File(context.cacheDir,"focus-colors.epub")
        instrumentation.context.assets.open("reader-interactions.epub").use {input->fixture.outputStream().use {input.copyTo(it)}}
        MobileCore(File(context.filesDir,"core").absolutePath).use {core->
            val book=core.importBook(fixture.absolutePath).first {it.title=="阅读交互测试"}
            val reader=core.openBook(book.id,Viewport(360u,640u,2f))
            try {
                for(dark in listOf(false,true))reader.configure(20f,false,dark).use {page->
                    val info=page.info().copy(focusBounds=listOf(Rect(10f,10f,20f,20f)))
                    assertEquals(140,info.focusFadeColor.toInt() ushr 24)
                    instrumentation.runOnMainSync {
                        val density=context.resources.displayMetrics.density
                        val size=(100*density).toInt()
                        val paper=info.focusFadeColor.toInt() or (0xFF shl 24)
                        val ink=if(dark)android.graphics.Color.WHITE else android.graphics.Color.BLACK
                        val bitmap=Bitmap.createBitmap(size,size,Bitmap.Config.ARGB_8888)
                        val output=Bitmap.createBitmap(size,size,Bitmap.Config.ARGB_8888)
                        try {
                            bitmap.eraseColor(paper)
                            val paint=android.graphics.Paint().apply {color=ink}
                            val input=android.graphics.Canvas(bitmap)
                            for(position in listOf(15f,70f))input.drawRect(position*density,position*density,(position+10)*density,(position+10)*density,paint)
                            val view=ReaderView(context)
                            view.layout(0,0,size,size)
                            view.present(ReaderFrame(bitmap,info),null)
                            view.draw(android.graphics.Canvas(output))
                            // Android's 8-bit alpha compositing can round a
                            // same-color blend down by one channel value.
                            val background=output.getPixel(0,0)
                            for(shift in listOf(0,8,16))assertTrue("Paper must retain its color",kotlin.math.abs(((paper ushr shift) and 255)-((background ushr shift) and 255))<=1)
                            assertEquals("Active content must retain full opacity",ink,output.getPixel((20*density).toInt(),(20*density).toInt()))
                            val faded=output.getPixel((75*density).toInt(),(75*density).toInt())
                            for(shift in listOf(0,8,16)) {
                                val expected=(((ink ushr shift) and 255)*115+((paper ushr shift) and 255)*140+127)/255
                                assertTrue("Inactive ink must fade toward its own paper",kotlin.math.abs(((faded ushr shift) and 255)-expected)<=2)
                            }
                            view.present(ReaderFrame(bitmap,info.copy(focusBounds=emptyList())),null)
                            view.draw(android.graphics.Canvas(output))
                            assertEquals("Disabling focus must restore content",ink,output.getPixel((75*density).toInt(),(75*density).toInt()))
                        }finally{bitmap.recycle();output.recycle()}
                    }
                }
            }finally{reader.shutdown();reader.close()}
        }
        fixture.delete()
        Unit
    }
    @Test fun legacyCredentialsCanBeImportedWithoutCreatingNewKeystoreKeys() {
        val context=InstrumentationRegistry.getInstrumentation().targetContext
        val preferencesName="credential-migration-fixture"
        val alias="torto.test.legacy-migration"
        val preferences=context.getSharedPreferences(preferencesName,0)
        val storage=LegacyKeystoreStorage(context,preferencesName,alias)
        val keys=java.security.KeyStore.getInstance("AndroidKeyStore").apply {load(null)}
        preferences.edit().clear().commit();keys.deleteEntry(alias)
        assertTrue(storage.readAll().isEmpty())
        assertFalse(keys.containsAlias(alias))
        val generator=javax.crypto.KeyGenerator.getInstance("AES","AndroidKeyStore")
        generator.init(android.security.keystore.KeyGenParameterSpec.Builder(alias,android.security.keystore.KeyProperties.PURPOSE_ENCRYPT or android.security.keystore.KeyProperties.PURPOSE_DECRYPT)
            .setBlockModes("GCM").setEncryptionPaddings("NoPadding").build())
        val key=generator.generateKey()
        val secret="fixture-secret-密钥"
        val name="a".repeat(64)
        try {
            val cipher=javax.crypto.Cipher.getInstance("AES/GCM/NoPadding").apply {init(javax.crypto.Cipher.ENCRYPT_MODE,key);updateAAD(name.toByteArray())}
            val encoded=android.util.Base64.encodeToString(cipher.iv+cipher.doFinal(secret.toByteArray()),android.util.Base64.NO_WRAP)
            assertTrue(preferences.edit().putString(name,encoded).commit())
            assertEquals(mapOf(name to secret),storage.readAll())
            assertEquals(encoded,preferences.getString(name,null))
            storage.clear()
            assertTrue(storage.readAll().isEmpty())
        } finally {preferences.edit().clear().commit();keys.deleteEntry(alias)}
    }
    @Test fun previewPinchZoomAndDoubleTapReset(){
        val instrumentation=InstrumentationRegistry.getInstrumentation()
        instrumentation.runOnMainSync {
            val image=Bitmap.createBitmap(64,64,Bitmap.Config.ARGB_8888).apply {eraseColor(android.graphics.Color.GREEN)}
            val view=ZoomImageView(instrumentation.targetContext,image)
            val minimum=if(android.os.Build.VERSION.SDK_INT>=29)android.view.ViewConfiguration.get(instrumentation.targetContext).scaledMinimumScalingSpan.toFloat() else 240f
            val initialSpan=(minimum+40f).coerceAtLeast(240f)
            val width=(initialSpan*1.5f+160f).toInt().coerceAtLeast(1080)
            val height=width*16/9
            val centerX=width/2f;val centerY=height/2f
            view.layout(0,0,width,height)
            fun snapshot()=Bitmap.createBitmap(width,height,Bitmap.Config.ARGB_8888).also {view.draw(android.graphics.Canvas(it))}
            val before=snapshot()
            var clock=android.os.SystemClock.uptimeMillis()
            var gestureStart=clock
            fun event(action:Int,points:List<Pair<Float,Float>>){
                if(action==android.view.MotionEvent.ACTION_DOWN)gestureStart=clock
                val properties=Array(points.size){index->android.view.MotionEvent.PointerProperties().apply {id=index;toolType=android.view.MotionEvent.TOOL_TYPE_FINGER}}
                val coords=Array(points.size){index->android.view.MotionEvent.PointerCoords().apply {x=points[index].first;y=points[index].second;pressure=1f;size=1f}}
                val event=android.view.MotionEvent.obtain(gestureStart,clock,action,points.size,properties,coords,0,0,1f,1f,0,0,android.view.InputDevice.SOURCE_TOUCHSCREEN,0)
                view.onTouchEvent(event);event.recycle();clock+=20
            }
            fun pointers(span:Float)=listOf((centerX-span/2) to centerY,(centerX+span/2) to centerY)
            event(android.view.MotionEvent.ACTION_DOWN,listOf(pointers(initialSpan)[0]))
            event(android.view.MotionEvent.ACTION_POINTER_DOWN or (1 shl 8),pointers(initialSpan))
            for(factor in listOf(1.1f,1.2f,1.3f,1.4f,1.5f))event(android.view.MotionEvent.ACTION_MOVE,pointers(initialSpan*factor))
            event(android.view.MotionEvent.ACTION_POINTER_UP or (1 shl 8),pointers(initialSpan*1.5f))
            event(android.view.MotionEvent.ACTION_UP,listOf(pointers(initialSpan*1.5f)[0]))
            val zoomed=snapshot()
            assertFalse("Pinch must change the rendered viewport",before.sameAs(zoomed))
            clock+=700
            repeat(2){event(android.view.MotionEvent.ACTION_DOWN,listOf(centerX to centerY));event(android.view.MotionEvent.ACTION_UP,listOf(centerX to centerY));clock+=50}
            val reset=snapshot();assertTrue("Double tap must restore fitted image",before.sameAs(reset))
            before.recycle();zoomed.recycle();reset.recycle();image.recycle()
        }
    }
    @Test fun focusLinksAndTouchTargetsUseNativeSourcePositions()=runBlocking {
        val instrumentation=InstrumentationRegistry.getInstrumentation()
        val context=instrumentation.targetContext
        val fixture=File(context.cacheDir,"reader-interactions.epub")
        instrumentation.context.assets.open("reader-interactions.epub").use {input->fixture.outputStream().use {input.copyTo(it)}}
        MobileCore(File(context.filesDir,"core").absolutePath).use {core->
            val book=core.importBook(fixture.absolutePath).first {it.title=="阅读交互测试"}
            val reader=core.openBook(book.id,Viewport(360u,640u,2f))
            try{
                reader.page().use {page->
                    assertTrue(page.info().regions.any {r->r.text.contains("Jump internally")})
                    val label=page.info().regions.first {it.text.contains("Inline formula:")}.bounds.last()
                    // Text ranges may include the inline object's source slot;
                    // probe its trailing neighbourhood instead of assuming the
                    // fraction lies strictly outside the text rectangle.
                    var formula:TouchAction?=null
                    search@ for(dy in -32..32 step 4)for(dx in -32..32 step 4){
                        val candidate=reader.activate(label.x+label.width+dx,label.y+label.height/2+dy)
                        if(candidate.kind=="formula"){formula=candidate;break@search}
                        candidate.media?.close()
                    }
                    assertNotNull("Missing formula touch target near $label",formula)
                    formula!!.media!!.use {preview->
                            val bitmap=Bitmap.createBitmap(preview.width().toInt(),preview.height().toInt(),Bitmap.Config.ARGB_8888)
                            try {
                                assertTrue(NativePixels.copyPage(preview.id().toLong(),bitmap))
                                val pixels=IntArray(bitmap.width*bitmap.height)
                                bitmap.getPixels(pixels,0,bitmap.width,0,0,bitmap.width,bitmap.height)
                                val paintedRows=(0 until bitmap.height).count {y->(0 until bitmap.width).any {x->pixels[y*bitmap.width+x] ushr 24 > 0}}
                                assertTrue("Fraction must include numerator and denominator, not only its rule",paintedRows>bitmap.height/4)
                            } finally {bitmap.recycle()}
                    }
                }
                reader.focusMode(true).use {assertTrue(it.info().focusCount>=2u);assertTrue(it.info().focusBounds.isNotEmpty())}
                reader.turn(true).use {assertEquals(1u,it.info().focusIndex)}
                reader.turn(false).use {assertEquals(0u,it.info().focusIndex)}
                reader.focusMode(false).use {assertTrue(it.info().focusBounds.isEmpty())}
                reader.followLink("body.xhtml#destination").use {assertTrue(it.info().canGoBack)}
                reader.goBack().use {assertFalse(it.info().canGoBack)}
                val control=ChatRequest()
                try{
                    control.cancel()
                    try{reader.translatePageCancellable(true,control).close();fail("Expected cancelled processing")}
                    catch(e:CoreException){assertTrue(e.message.orEmpty().contains("已停止"))}
                }finally{control.close()}
            }finally{reader.shutdown();reader.close()}
        }
        fixture.delete()
        Unit
    }
    @Test fun sharedCoreRendersSearchesSelectsAndRestores() = runBlocking {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val fixture = File(context.cacheDir,"android-integration.fb2")
        val paragraphs = (0..60).joinToString("") { "<p>Paragraph $it: A shared reading engine keeps source locations stable. 中文排版与搜索。</p>" }
        fixture.writeText("""<?xml version="1.0" encoding="UTF-8"?><FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0"><description><title-info><book-title>阅读引擎测试</book-title><lang>en</lang></title-info></description><body><section id="one"><title><p>Chapter One</p></title>$paragraphs</section></body></FictionBook>""")
        MobileCore(File(context.filesDir,"core").absolutePath).use { core ->
            val book = core.importBook(fixture.absolutePath).first { it.title == "阅读引擎测试" }
            val viewport=Viewport(360u,640u,2f)
            val reader=core.openBook(book.id,viewport)
            try {
                reader.page().use { page ->
                    assertEquals(720u,page.width()); assertEquals(1280u,page.height())
                    val bitmap=Bitmap.createBitmap(720,1280,Bitmap.Config.ARGB_8888)
                    assertTrue(NativePixels.copyPage(page.id().toLong(),bitmap))
                    assertFalse(NativePixels.copyPage(Long.MAX_VALUE,bitmap))
                    val mismatched=Bitmap.createBitmap(1,1,Bitmap.Config.ARGB_8888)
                    assertFalse(NativePixels.copyPage(page.id().toLong(),mismatched))
                    assertTrue(page.info().regions.any { it.text.contains("Paragraph") })
                }
                val hit=reader.search("Paragraph 45").single()
                reader.goToSource(hit.source).use { page ->
                    val region=page.info().regions.first { it.text.contains("Paragraph 45") }
                    val rect=region.bounds.first()
                    val selected=reader.select(rect.x+2f,rect.y+rect.height/2,rect.x+45f,rect.y+rect.height/2)
                    assertNotNull(selected); assertTrue(selected!!.text.isNotBlank())
                    reader.selectionMode("paragraph")
                    val notes=reader.saveAnnotation("设备端批注测试")
                    val note=notes.last { it.note=="设备端批注测试" }
                    reader.page().use { assertTrue(it.info().marks.isNotEmpty()) }
                    reader.editAnnotation(note.id,"edited on device")
                    assertEquals("edited on device",reader.annotations().first { it.id==note.id }.note)
                    reader.clearSelection()
                    reader.deleteAnnotation(note.id)
                    val request=ChatRequest()
                    try {
                        request.cancel()
                        try { reader.chat("test cancellation",request,object:ChatListener { override fun update(content:String,reasoning:String,activity:String) { fail("cancelled callback") } });fail("Expected cancellation") }
                        catch(e:CoreException) { assertTrue(e.message.orEmpty().contains("已停止")) }
                    } finally { request.close() }
                    assertEquals(7,core.statistics().distribution.size)
                }
                reader.resize(Viewport(600u,360u,2f)).use { page -> assertEquals(1200u,page.width()) }
            } finally {reader.shutdown();reader.close()}
            core.openBook(book.id,viewport).use { reopened ->
                reopened.page().use { page -> assertTrue(page.info().progress>0) }
                reopened.shutdown()
            }
        }
        fixture.delete()
        Unit
    }
}

package org.tortotech.torto

import android.graphics.Bitmap
import android.graphics.Rect
import android.view.View
import android.view.ViewGroup
import android.view.accessibility.AccessibilityNodeInfo
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.Modifier
import androidx.lifecycle.ViewModelProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.sync.Mutex
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.tortotech.torto.core.BookSummary
import java.io.File

@RunWith(AndroidJUnit4::class)
class ReaderOpeningLoadingTest {
    private val instrumentation get()=InstrumentationRegistry.getInstrumentation()
    private fun await(message:String,condition:()->Boolean){
        val until=android.os.SystemClock.uptimeMillis()+8000
        while(!condition()){assertTrue(message,android.os.SystemClock.uptimeMillis()<until);android.os.SystemClock.sleep(25)}
    }
    private fun progress():List<AccessibilityNodeInfo> {
        instrumentation.uiAutomation.clearCache()
        val out=mutableListOf<AccessibilityNodeInfo>()
        fun visit(node:AccessibilityNodeInfo?){if(node==null)return
            if(node.isVisibleToUser&&node.className=="android.widget.ProgressBar")out.add(node)
            for(i in 0 until node.childCount)visit(node.getChild(i))
        }
        visit(instrumentation.uiAutomation.rootInActiveWindow);return out
    }
    private fun opening()=progress().firstOrNull {it.contentDescription?.toString() in listOf("正在打开书籍","Opening book")}
    private fun reader(view:View):ReaderView?=if(view is ReaderView)view else if(view is ViewGroup)(0 until view.childCount).firstNotNullOfOrNull {reader(view.getChildAt(it))}else null
    private fun launch():Pair<android.app.Instrumentation.ActivityMonitor,MainActivity>{
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
        val activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity
        assertNotNull(activity);return monitor to activity!!
    }
    @Test fun openingFocusUsesTheFinalViewportFromTheFirstVisibleFrame():Unit=runBlocking {
        val context=instrumentation.targetContext;val core=NativeRuntime.core(context)
        val prefs=context.getSharedPreferences("reading",0);val originalFocus=prefs.getBoolean("focus",false)
        val originalAi=core.aiOptions();val before=core.books().map {it.id}.toSet()
        val file=File(context.cacheDir,"opening-viewport.fb2")
        file.writeText("""<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0"><description><title-info><genre>nonfiction</genre><book-title>Opening viewport</book-title><lang>en</lang></title-info></description><body><section>${(0..30).joinToString(""){"<p>Opening paragraph $it stays centred while the book becomes visible.</p>"}}</section></body></FictionBook>""")
        val book=core.importBook(file.path).single {it.title=="Opening viewport"}
        check(prefs.edit().putBoolean("focus",true).commit())
        core.saveAiOptions(originalAi.copy(layoutEnabled=false))
        val (monitor,activity)=launch();var model:ReaderModel?=null
        val samples=mutableListOf<String>();val offsets=mutableListOf<Float>()
        val visibleFrame=ReaderView::class.java.getDeclaredField("frame").apply {isAccessible=true}
        val observer=android.view.ViewTreeObserver.OnPreDrawListener {
            val view=reader(activity.window.decorView)
            val frame=view?.let {visibleFrame.get(it) as? ReaderFrame}
            if(model?.state?.value?.active?.id==book.id&&frame?.info?.isScroll==true){
                frame.info.focusBounds.firstOrNull()?.let {bounds->
                    val density=view!!.resources.displayMetrics.density
                    val offset=(bounds.y+bounds.height/2)*density-view.height/2f
                    samples.add("view=${view.width}x${view.height} focusOffset=$offset raster=${frame.rasterId}")
                    offsets.add(offset)
                }
            }
            true
        }
        try {
            instrumentation.runOnMainSync {model=ViewModelProvider(activity)[ReaderModel::class.java]}
            await("shelf initializes"){model!!.state.value.loaded&&!model!!.state.value.busy}
            instrumentation.runOnMainSync {activity.window.decorView.viewTreeObserver.addOnPreDrawListener(observer);model!!.open(book)}
            await("focus content appears"){model!!.state.value.frame?.info?.isScroll==true&&!model!!.state.value.busy}
            android.os.SystemClock.sleep(600)
            instrumentation.runOnMainSync {activity.window.decorView.viewTreeObserver.removeOnPreDrawListener(observer)}
            File(context.cacheDir,"opening-viewport-trace.txt").writeText(samples.distinct().joinToString("\n"))
            assertTrue("The test observes visible content",offsets.isNotEmpty())
            assertTrue("First content must use the final immersive height: ${samples.distinct()}",offsets.all {kotlin.math.abs(it)<4f})
        }finally {
            instrumentation.runOnMainSync {activity.window.decorView.viewTreeObserver.removeOnPreDrawListener(observer);model?.back();activity.finish()}
            instrumentation.removeMonitor(monitor)
            check(prefs.edit().putBoolean("focus",originalFocus).commit())
            core.saveAiOptions(originalAi);if(book.id !in before)core.removeBook(book.id);file.delete()
        }
    }
    @Test fun openingUsesOnlyCenteredProgressAndStopsAfterContentErrorOrExit(){
        val (monitor,activity)=launch()
        val state=mutableStateOf(ScreenState(active=BookSummary(0.0,0u,0u,"book","Book",emptyList()),openingBook=false,busy=false,language="en"))
        try {
            instrumentation.runOnMainSync {activity.setContent {TortoTheme(state.value){
                Box(Modifier.fillMaxSize()){
                    ReaderOpeningIndicator(state.value)
                }
            }}}
            instrumentation.waitForIdleSync()
            // A completed fast open cannot publish a delayed progress frame.
            instrumentation.runOnMainSync {state.value=state.value.copy(openingBook=true,busy=true)}
            android.os.SystemClock.sleep(50)
            instrumentation.runOnMainSync {state.value=state.value.copy(openingBook=false,busy=false)}
            android.os.SystemClock.sleep(300)
            assertTrue(progress().isEmpty())
            for(stop in listOf("content","error","exit")){
                instrumentation.runOnMainSync {state.value=state.value.copy(active=BookSummary(0.0,0u,0u,"book-$stop","Book",emptyList()),openingBook=true,busy=true,error=null)}
                await("slow opening shows one circular indicator"){opening()!=null}
                assertEquals("Opening never also shows the header progress bar",1,progress().size)
                val bounds=Rect().also(opening()!!::getBoundsInScreen)
                assertEquals("The indicator is circular",bounds.width(),bounds.height())
                instrumentation.runOnMainSync {state.value=when(stop){
                    "error"->state.value.copy(openingBook=false,busy=false,error="Unavailable")
                    "exit"->state.value.copy(active=null,openingBook=false,busy=false)
                    else->state.value.copy(openingBook=false,busy=false)
                }}
                await("completion/error/exit removes the spinner immediately"){progress().isEmpty()}
            }
        }finally {instrumentation.runOnMainSync {activity.finish()};instrumentation.removeMonitor(monitor)}
    }
    @Test fun slowBookOpeningKeepsFeedbackWhileQueuedThenHidesOnFirstFrame():Unit=runBlocking {
        val context=instrumentation.targetContext;val core=NativeRuntime.core(context)
        val originalAi=core.aiOptions();val before=core.books().map {it.id}.toSet()
        val file=File(context.cacheDir,"opening-feedback.fb2")
        file.writeText("""<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0"><description><title-info><genre>nonfiction</genre><book-title>Opening feedback</book-title><lang>en</lang></title-info></description><body><section><title><p>Opening feedback</p></title><p>The first visible paragraph finishes opening the book.</p></section></body></FictionBook>""")
        val book=core.importBook(file.absolutePath).single {it.title=="Opening feedback"}
        val (monitor,activity)=launch();var held:Mutex?=null
        var model:ReaderModel?=null
        try {
            core.saveAiOptions(originalAi.copy(layoutEnabled=false))
            instrumentation.runOnMainSync {model=ViewModelProvider(activity)[ReaderModel::class.java]}
            await("model initialized"){model!!.state.value.loaded&&!model!!.state.value.busy}
            // Exercise the actual global operation lifecycle on the shelf.
            // Holding a controlled operation makes the old 200ms header bar
            // deterministic without delaying any native or network service.
            val release=kotlinx.coroutines.CompletableDeferred<Unit>()
            val slowOperation:suspend ()->Unit={release.await()}
            val runOperation=ReaderModel::class.java.declaredMethods.single {it.name=="runOperation"}.apply {isAccessible=true}
            try {
                instrumentation.runOnMainSync {runOperation.invoke(model,false,slowOperation)}
                await("ordinary operation enters busy state"){model!!.state.value.busy}
                android.os.SystemClock.sleep(400)
                assertTrue("Slow shelf operations never add a global header progress bar",progress().isEmpty())
            }finally {release.complete(Unit)}
            await("ordinary operation completes"){!model!!.state.value.busy}
            val mutex=ReaderModel::class.java.getDeclaredField("operations").apply {isAccessible=true}.get(model) as Mutex
            mutex.lock();held=mutex
            instrumentation.runOnMainSync {model!!.open(book)}
            await("queued book opening still gets visible feedback"){opening()!=null}
            assertTrue(model!!.state.value.openingBook)
            assertNull(model!!.state.value.frame)
            assertEquals(1,progress().size)
            val circle=Rect().also(opening()!!::getBoundsInScreen)
            val viewport=Rect()
            instrumentation.runOnMainSync {reader(activity.window.decorView)!!.let {view->val at=IntArray(2);view.getLocationOnScreen(at);viewport.set(at[0],at[1],at[0]+view.width,at[1]+view.height)}}
            assertEquals(viewport.exactCenterX(),circle.exactCenterX(),3f)
            assertEquals(viewport.exactCenterY(),circle.exactCenterY(),3f)
            android.os.SystemClock.sleep(250)
            instrumentation.uiAutomation.takeScreenshot()?.let {image->
                try {
                    val ink=(circle.top until circle.bottom).sumOf {y->(circle.left until circle.right).count {x->
                        val pixel=image.getPixel(x,y);val green=android.graphics.Color.green(pixel)
                        green>android.graphics.Color.red(pixel)+20&&green>android.graphics.Color.blue(pixel)+10
                    }}
                    assertTrue("The centred indicator paints visible green ink",ink>10)
                    File(context.cacheDir,"book-opening-centered.png").outputStream().use {image.compress(Bitmap.CompressFormat.PNG,100,it)}
                }finally {image.recycle()}
            }
            mutex.unlock();held=null
            await("native first frame completes opening"){model!!.state.value.frame!=null&&!model!!.state.value.openingBook&&!model!!.state.value.busy}
            await("first frame removes loading feedback"){progress().isEmpty()}
            instrumentation.runOnMainSync {model!!.back()}
            await("reader closes"){model!!.state.value.active==null&&!model!!.state.value.busy}
            instrumentation.runOnMainSync {model!!.open(book.copy(id="missing-opening-book"))}
            await("a failed open clears loading state"){model!!.state.value.error!=null&&!model!!.state.value.openingBook&&!model!!.state.value.busy}
            assertNull(opening())
        }finally {
            held?.unlock()
            instrumentation.runOnMainSync {model?.back();activity.finish()};instrumentation.removeMonitor(monitor)
            core.saveAiOptions(originalAi);if(book.id !in before)core.removeBook(book.id);file.delete()
        }
    }
}

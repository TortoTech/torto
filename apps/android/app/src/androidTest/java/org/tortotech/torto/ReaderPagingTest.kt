package org.tortotech.torto

import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.view.MotionEvent
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.tortotech.torto.core.PageInfo
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

@RunWith(AndroidJUnit4::class)
class ReaderPagingTest {
    @Test fun classicDragPreviewsCancelsCommitsAndStopsDrawing()=verifyPaging(false)
    @Test fun focusUnitDragUsesPreparedPagesAndOldAppSlide()=verifyPaging(true)
    private fun verifyPaging(focus:Boolean){
        val instrumentation=InstrumentationRegistry.getInstrumentation()
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        var activity:MainActivity?=null
        val bitmaps=mutableListOf<Bitmap>()
        try {
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity
            assertNotNull(activity)
            lateinit var view:ReaderView
            instrumentation.runOnMainSync {view=ReaderView(activity!!);activity!!.setContentView(view)}
            instrumentation.waitForIdleSync();android.os.SystemClock.sleep(300)
            val turns=AtomicInteger();val draws=AtomicInteger()
            lateinit var first:ReaderFrame;lateinit var second:ReaderFrame
            var settled=CountDownLatch(1)
            instrumentation.runOnMainSync {
                val info=PageInfo(section=0u,sectionCount=1u,page=0u,pageCount=3u,progress=0.0,
                    regions=emptyList(),marks=emptyList(),focusBounds=if(focus)listOf(org.tortotech.torto.core.Rect(0f,0f,view.width/view.resources.displayMetrics.density,view.height/view.resources.displayMetrics.density))else emptyList(),focusFadeColor=0x8CFFFFFFu,
                    focusHighlight=false,isScroll=focus,scrollDistance=0.0,scrollContext=1uL,rasterTop=0f,
                    rasterHeight=view.height/view.resources.displayMetrics.density,previousFocus=null,nextFocus=null,
                    controls=emptyList(),activeTocId=null,activeTocPath=emptyList(),focusIndex=0u,focusCount=if(focus)1u else 0u,canGoBack=false)
                fun frame(color:Int,index:UInt)=ReaderFrame(Bitmap.createBitmap(view.width,view.height,Bitmap.Config.ARGB_8888).apply {eraseColor(color);bitmaps.add(this)},info.copy(page=index,scrollContext=index.toULong()+1uL),index.toULong()+1uL)
                first=frame(Color.RED,0u);second=frame(Color.BLUE,1u)
                view.present(first,null,null,second)
                view.onMotion={if(!it)settled.countDown()}
                val navigate:(Boolean)->Unit={assertTrue(it);turns.incrementAndGet();view.present(second,null,first,null)}
                if(focus){view.onUnitTurn=navigate;view.onTurn={fail("Horizontal focus gestures must navigate units")}}else view.onTurn=navigate
                val start=android.os.SystemClock.uptimeMillis()
                fun event(action:Int,x:Float,time:Long){MotionEvent.obtain(start,time,action,x,view.height*.5f,0).also {view.onTouchEvent(it);it.recycle()}}
                event(MotionEvent.ACTION_DOWN,view.width*.8f,start)
                event(MotionEvent.ACTION_MOVE,view.width*.4f,start+80)
                assertEquals("Dragging alone never changes the source page",0,turns.get())
                val image=Bitmap.createBitmap(view.width,view.height,Bitmap.Config.ARGB_8888)
                try {
                    view.draw(Canvas(image))
                    assertEquals("Current page follows the finger",Color.RED,image.getPixel(2,view.height/2))
                    assertEquals("The prepared next page is visible during dragging",Color.BLUE,image.getPixel(view.width-3,view.height/2))
                }finally{image.recycle()}
                event(MotionEvent.ACTION_CANCEL,view.width*.4f,start+200)
            }
            assertTrue("Cancelled slide returns to the current page",settled.await(2,TimeUnit.SECONDS));assertEquals(0,turns.get())
            settled=CountDownLatch(1)
            instrumentation.runOnMainSync {
                val start=android.os.SystemClock.uptimeMillis()
                fun event(action:Int,x:Float,time:Long){MotionEvent.obtain(start,time,action,x,view.height*.5f,0).also {view.onTouchEvent(it);it.recycle()}}
                event(MotionEvent.ACTION_DOWN,view.width*.8f,start)
                event(MotionEvent.ACTION_MOVE,view.width*.4f,start+100)
                event(MotionEvent.ACTION_UP,view.width*.4f,start+400)
            }
            assertTrue("Committed slide reaches a stable endpoint",settled.await(2,TimeUnit.SECONDS));assertEquals(1,turns.get())
            settled=CountDownLatch(1)
            instrumentation.runOnMainSync {
                val navigate:(Boolean)->Unit={assertFalse(it);turns.incrementAndGet();view.present(first,null,null,second)}
                if(focus)view.onUnitTurn=navigate else view.onTurn=navigate
                val start=android.os.SystemClock.uptimeMillis()
                fun event(action:Int,x:Float,time:Long){MotionEvent.obtain(start,time,action,x,view.height*.5f,0).also {view.onTouchEvent(it);it.recycle()}}
                event(MotionEvent.ACTION_DOWN,view.width*.2f,start)
                event(MotionEvent.ACTION_MOVE,view.width*.6f,start+80)
                assertEquals(1,turns.get())
                val image=Bitmap.createBitmap(view.width,view.height,Bitmap.Config.ARGB_8888)
                try {view.draw(Canvas(image));assertEquals(Color.RED,image.getPixel(2,view.height/2));assertEquals(Color.BLUE,image.getPixel(view.width-3,view.height/2))}finally{image.recycle()}
                event(MotionEvent.ACTION_UP,view.width*.6f,start+400)
            }
            assertTrue("Previous units slide in from the opposite side",settled.await(2,TimeUnit.SECONDS));assertEquals(2,turns.get())
            android.os.SystemClock.sleep(250)
            instrumentation.runOnMainSync {view.viewTreeObserver.addOnDrawListener {draws.incrementAndGet()}}
            instrumentation.waitForIdleSync();draws.set(0);android.os.SystemClock.sleep(250)
            assertEquals("Settled paging has no redraw loop",0,draws.get())
        }finally {
            instrumentation.runOnMainSync {activity?.finish()};instrumentation.removeMonitor(monitor)
            bitmaps.forEach {it.recycle()}
        }
    }
}

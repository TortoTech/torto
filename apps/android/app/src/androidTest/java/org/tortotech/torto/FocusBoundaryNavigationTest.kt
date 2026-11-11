package org.tortotech.torto

import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.os.SystemClock
import android.view.MotionEvent
import android.view.ViewConfiguration
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.tortotech.torto.core.FocusTarget
import org.tortotech.torto.core.PageInfo
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger

@RunWith(AndroidJUnit4::class)
class FocusBoundaryNavigationTest {
    private val instrumentation get()=InstrumentationRegistry.getInstrumentation()
    private fun info(boundary:Boolean,height:Float=640f)=PageInfo(section=0u,sectionCount=3u,page=0u,pageCount=1u,progress=0.0,
        regions=emptyList(),marks=emptyList(),focusBounds=emptyList(),focusFadeColor=0u,focusHighlight=false,isScroll=true,
        scrollDistance=0.0,scrollContext=333uL,rasterTop=-160f,rasterHeight=height+320f,
        previousFocus=FocusTarget(-32f,emptyList()),nextFocus=if(boundary)null else FocusTarget(32f,emptyList()),
        controls=emptyList(),activeTocId="one",activeTocPath=emptyList(),focusIndex=0u,focusCount=3u,canGoBack=false)
    private fun event(view:ReaderView,start:Long,action:Int,y:Float,elapsed:Long){
        MotionEvent.obtain(start,start+elapsed,action,view.width/2f,y,0).also {view.onTouchEvent(it);it.recycle()}
    }
    @Test fun shortGesturesConfirmBothParagraphsAndSectionEdges(){
        instrumentation.runOnMainSync {
            val context=instrumentation.targetContext;val density=context.resources.displayMetrics.density
            val width=(360*density).toInt();val height=(640*density).toInt()
            val bitmap=Bitmap.createBitmap(width,(960*density).toInt(),Bitmap.Config.ARGB_8888)
            try {for(boundary in listOf(false,true)){
                val view=ReaderView(context);view.layout(0,0,width,height);view.present(ReaderFrame(bitmap,info(boundary),333uL),null)
                val turns=AtomicInteger();view.onTurn={assertTrue(it);turns.incrementAndGet()}
                val start=SystemClock.uptimeMillis();val y=320*density;val delta=18*density+ViewConfiguration.get(context).scaledTouchSlop
                event(view,start,0,y,0);event(view,start,2,y-delta,140);event(view,start,1,y-delta,320)
                assertEquals("The same short gesture commits at the section edge too",1,turns.get())
                view.cancelPendingNavigation();view.onWindowFocusChanged(false)
            }}finally {bitmap.recycle()}
        }
    }
    @Test fun pendingUnitTurnsKeepOneGestureIncludingAReplyBeforeFingerRelease(){
        instrumentation.runOnMainSync {
            val context=instrumentation.targetContext;val density=context.resources.displayMetrics.density
            val width=(360*density).toInt();val height=(640*density).toInt()
            val bitmap=Bitmap.createBitmap(width,(960*density).toInt(),Bitmap.Config.ARGB_8888)
            try {for(boundary in listOf(false,true))for(replyWhileDown in listOf(false,true))for(cancelled in listOf(false,true))for(contextId in listOf(333uL,334uL)){
                val view=ReaderView(context);view.layout(0,0,width,height);val first=info(boundary)
                view.present(ReaderFrame(bitmap,first,333uL),null)
                val turns=AtomicInteger();val moving=AtomicBoolean();view.onTurn={assertTrue(it);turns.incrementAndGet()};view.onMotion={moving.set(it)}
                var start=SystemClock.uptimeMillis();val y=320*density;val delta=72*density+ViewConfiguration.get(context).scaledTouchSlop
                event(view,start,0,y,0);event(view,start,2,y-delta,140);event(view,start,1,y-delta,320)
                assertEquals(1,turns.get());start+=500
                event(view,start,0,y,0);event(view,start,2,y-delta,140)
                val second=first.copy(scrollContext=contextId,scrollDistance=if(boundary)0.0 else 32.0,
                    rasterTop=if(boundary)-160f else -192f,nextFocus=FocusTarget(32f,emptyList()))
                fun confirm(){view.present(ReaderFrame(bitmap,second,334uL),null,turnCompletion=1)}
                if(replyWhileDown)confirm()
                event(view,start,if(cancelled)3 else 1,y-delta,320)
                if(!replyWhileDown)confirm()
                assertEquals("boundary=$boundary context=$contextId replyWhileDown=$replyWhileDown cancelled=$cancelled",if(cancelled)1 else 2,turns.get())
                view.present(ReaderFrame(bitmap,second.copy(scrollDistance=second.scrollDistance+32,rasterTop=second.rasterTop-32),335uL),null,turnCompletion=2)
                assertFalse("All native confirmations release navigation",moving.get())
                view.cancelPendingNavigation();view.onWindowFocusChanged(false)
            }}finally {bitmap.recycle()}
        }
    }
    @Test fun aHorizontalGestureDuringAnUnfinishedBoundaryTurnIsKeptOnce(){
        instrumentation.runOnMainSync {
            val context=instrumentation.targetContext;val density=context.resources.displayMetrics.density
            val width=(360*density).toInt();val height=(640*density).toInt()
            val bitmap=Bitmap.createBitmap(width,(960*density).toInt(),Bitmap.Config.ARGB_8888)
            try {for(replyWhileDown in listOf(false,true))for(cancelled in listOf(false,true)){
                val view=ReaderView(context);view.layout(0,0,width,height);val first=info(true)
                view.present(ReaderFrame(bitmap,first,333uL),null)
                val focusTurns=AtomicInteger();val unitTurns=AtomicInteger();val moving=AtomicBoolean()
                view.onTurn={assertTrue(it);focusTurns.incrementAndGet()};view.onUnitTurn={assertTrue(it);unitTurns.incrementAndGet()};view.onMotion={moving.set(it)}
                var start=SystemClock.uptimeMillis();val y=320*density;val delta=72*density+ViewConfiguration.get(context).scaledTouchSlop
                event(view,start,0,y,0);event(view,start,2,y-delta,140);event(view,start,1,y-delta,320)
                assertEquals(1,focusTurns.get());start+=500
                fun horizontal(action:Int,x:Float,time:Long){MotionEvent.obtain(start,start+time,action,x,y,0).also {view.onTouchEvent(it);it.recycle()}}
                horizontal(0,width*.8f,0);horizontal(2,width*.2f,140)
                val second=first.copy(scrollContext=334uL,nextFocus=FocusTarget(32f,emptyList()))
                fun confirm(){view.present(ReaderFrame(bitmap,second,334uL),null,turnCompletion=1)}
                if(replyWhileDown)confirm()
                horizontal(if(cancelled)3 else 1,width*.2f,320)
                if(!replyWhileDown)confirm()
                assertEquals(if(cancelled)0 else 1,unitTurns.get());assertEquals(1,focusTurns.get())
                view.present(ReaderFrame(bitmap,second.copy(scrollContext=335uL),335uL),null,turnCompletion=2)
                assertFalse(moving.get());view.cancelPendingNavigation();view.onWindowFocusChanged(false)
            }}finally {bitmap.recycle()}
        }
    }
    @Test fun sectionEdgeSlidesToAPlaceholderAndClearsLoadingAfterConfirmation(){
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        var activity:MainActivity?=null;var bitmap:Bitmap?=null
        try {
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity;assertNotNull(activity)
            lateinit var view:ReaderView
            instrumentation.runOnMainSync {view=ReaderView(activity!!);activity!!.setContentView(view)}
            instrumentation.waitForIdleSync();SystemClock.sleep(200)
            val loading=AtomicBoolean();val turns=AtomicInteger();val moving=AtomicBoolean()
            lateinit var first:PageInfo
            instrumentation.runOnMainSync {
                val density=view.resources.displayMetrics.density;val height=view.height/density
                bitmap=Bitmap.createBitmap(view.width,view.height+(320*density).toInt(),Bitmap.Config.ARGB_8888).apply {eraseColor(Color.BLUE)}
                first=info(true,height);view.present(ReaderFrame(bitmap!!,first,333uL),null)
                view.onTurn={assertTrue(it);turns.incrementAndGet()};view.onContentLoading={loading.set(it)};view.onMotion={moving.set(it)}
                val start=SystemClock.uptimeMillis();val y=view.height*.6f;val delta=72*density+ViewConfiguration.get(view.context).scaledTouchSlop
                event(view,start,0,y,0);event(view,start,2,y-delta,140);event(view,start,1,y-delta,320)
                assertTrue("Loading begins immediately after confirmation",loading.get())
                assertEquals("The outgoing animation precedes the native turn",0,turns.get())
            }
            val until=SystemClock.uptimeMillis()+2000
            while(turns.get()==0){assertTrue(SystemClock.uptimeMillis()<until);SystemClock.sleep(10)}
            assertTrue(loading.get());assertTrue(moving.get())
            instrumentation.runOnMainSync {
                val output=Bitmap.createBitmap(view.width,view.height,Bitmap.Config.ARGB_8888)
                try {view.draw(Canvas(output));assertEquals("Retained overscan must not leak into the loading placeholder",Color.BLACK,output.getPixel(view.width/2,view.height/2))}finally {output.recycle()}
                bitmap!!.eraseColor(Color.GREEN)
                view.present(ReaderFrame(bitmap!!,first.copy(scrollContext=334uL,nextFocus=FocusTarget(32f,emptyList())),334uL),null,turnCompletion=1)
                assertFalse(loading.get());assertFalse(moving.get())
                val result=Bitmap.createBitmap(view.width,view.height,Bitmap.Config.ARGB_8888)
                try {view.draw(Canvas(result));assertEquals(Color.GREEN,result.getPixel(view.width/2,view.height/2))}finally {result.recycle()}
            }
        }finally {instrumentation.runOnMainSync {activity?.finish()};instrumentation.removeMonitor(monitor);bitmap?.recycle()}
    }
}

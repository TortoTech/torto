package org.tortotech.torto

import android.animation.ValueAnimator
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
import org.tortotech.torto.core.PageInfo

@RunWith(AndroidJUnit4::class)
class ReaderFadeTransitionTest {
    private val instrumentation get()=InstrumentationRegistry.getInstrumentation()
    private fun fade(view:ReaderView)=ReaderView::class.java.getDeclaredField("contentFade").apply {isAccessible=true}.get(view) as? ValueAnimator
    private fun pixel(view:ReaderView):Int {
        val image=Bitmap.createBitmap(view.width,view.height,Bitmap.Config.ARGB_8888)
        return try {view.draw(Canvas(image));image.getPixel(view.width/2,view.height/2)}finally {image.recycle()}
    }
    private fun swipe(view:ReaderView,forward:Boolean=true){
        val start=SystemClock.uptimeMillis();val y=view.height*.5f
        val delta=(72*view.resources.displayMetrics.density+ViewConfiguration.get(view.context).scaledTouchSlop)*(if(forward)1 else -1)
        for((action,position,time)in listOf(Triple(0,y,0L),Triple(2,y-delta,140L),Triple(1,y-delta,320L)))
            MotionEvent.obtain(start,start+time,action,view.width*.5f,position,0).also {view.onTouchEvent(it);it.recycle()}
    }
    private fun attached(test:(ReaderView,MutableList<Bitmap>)->Unit){
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        var activity:MainActivity?=null;val images=mutableListOf<Bitmap>()
        try {
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity;assertNotNull(activity)
            lateinit var view:ReaderView
            instrumentation.runOnMainSync {view=ReaderView(activity!!);activity!!.setContentView(view)}
            instrumentation.waitForIdleSync();SystemClock.sleep(200)
            instrumentation.runOnMainSync {assertTrue(view.hasWindowFocus());assertTrue(ValueAnimator.areAnimatorsEnabled());test(view,images)}
        }finally {instrumentation.runOnMainSync {activity?.finish()};instrumentation.removeMonitor(monitor);images.forEach {it.recycle()}}
    }
    private fun frame(view:ReaderView,images:MutableList<Bitmap>,color:Int,context:ULong):ReaderFrame {
        val height=view.height/view.resources.displayMetrics.density
        val bitmap=Bitmap.createBitmap(view.width,view.height,Bitmap.Config.ARGB_8888).apply {eraseColor(color);images.add(this)}
        return ReaderFrame(bitmap,PageInfo(section=context.toUInt(),sectionCount=10u,page=0u,pageCount=1u,progress=0.0,
            regions=emptyList(),marks=emptyList(),focusBounds=emptyList(),focusFadeColor=0u,focusHighlight=false,
            isScroll=true,scrollDistance=0.0,scrollContext=context,rasterTop=0f,rasterHeight=height,
            previousFocus=null,nextFocus=null,controls=emptyList(),activeTocId=null,activeTocPath=emptyList(),focusIndex=0u,focusCount=1u,canGoBack=false),context)
    }
    @Test fun immediateReplyWaitsForFadeAndRefreshDoesNotReplayIt()=attached {view,images->
        val first=frame(view,images,Color.BLUE,1uL);val next=frame(view,images,Color.GREEN,2uL)
        view.present(first,null)
        var turns=0;var moving=false;var loading=false
        view.onMotion={moving=it};view.onContentLoading={loading=it}
        view.onTurn={assertTrue(it);turns++;view.present(next,null,turnCompletion=1)}
        swipe(view)
        assertEquals("Native work begins without waiting for the fade",1,turns)
        assertEquals("A cached reply must not replace still-visible outgoing content",Color.BLUE,pixel(view))
        val out=fade(view)!!;assertTrue(out.duration<=60);out.currentPlayTime=out.duration/2
        val partial=pixel(view);assertTrue(Color.blue(partial) in 1..254);assertEquals(0,Color.green(partial))
        out.end();assertFalse(loading);assertTrue(moving);assertEquals(Color.BLACK,pixel(view))
        val incoming=fade(view)!!;assertEquals(90L,incoming.duration);incoming.currentPlayTime=45
        assertTrue(Color.green(pixel(view)) in 1..254)
        incoming.end();assertFalse(moving);assertEquals(Color.GREEN,pixel(view))
        view.present(next.copy(rasterId=3uL),null,turnCompletion=1)
        assertNull("Translation/layout and other same-unit refreshes must not fade",fade(view))
        assertEquals(Color.GREEN,pixel(view))
    }
    @Test fun failedAndNoOpTurnsRestoreContentAndReleaseNavigation()=attached {view,images->
        val first=frame(view,images,Color.BLUE,1uL);view.present(first,null)
        var moving=false;var loading=false;var turns=0
        view.onMotion={moving=it};view.onContentLoading={loading=it};view.onTurn={turns++}
        swipe(view);fade(view)!!.end();assertEquals(Color.BLACK,pixel(view));assertTrue(loading)
        view.cancelPendingNavigation();assertFalse(moving);assertFalse(loading);assertEquals(Color.BLUE,pixel(view))
        view.onTurn={turns++;view.present(first,null,turnCompletion=1)}
        swipe(view);fade(view)!!.end();fade(view)!!.end()
        assertEquals(2,turns);assertFalse(moving);assertFalse(loading);assertEquals(Color.BLUE,pixel(view))
        // The reverse edge follows the same path and survives focus loss with a cached reply.
        val previous=frame(view,images,Color.RED,0uL)
        view.onTurn={assertFalse(it);view.present(previous,null,turnCompletion=2)}
        swipe(view,false);view.onWindowFocusChanged(false)
        assertNull(fade(view));assertFalse(moving);assertEquals(Color.RED,pixel(view))
    }
    @Test fun firstFrameFadesOnceAndGestureSettlesIt()=attached {view,images->
        val first=frame(view,images,Color.GREEN,1uL)
        view.animateOpening=true;view.present(first,null)
        assertEquals(Color.BLACK,pixel(view));assertNotNull(fade(view))
        val time=SystemClock.uptimeMillis()
        MotionEvent.obtain(time,time,0,view.width*.5f,view.height*.5f,0).also {view.onTouchEvent(it);it.recycle()}
        assertNull(fade(view));assertEquals(Color.GREEN,pixel(view))
        MotionEvent.obtain(time,time+30,3,view.width*.5f,view.height*.5f,0).also {view.onTouchEvent(it);it.recycle()}
        view.present(first.copy(rasterId=2uL),null)
        assertNull(fade(view))
    }
}

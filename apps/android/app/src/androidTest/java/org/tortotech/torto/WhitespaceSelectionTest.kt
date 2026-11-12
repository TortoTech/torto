package org.tortotech.torto

import android.graphics.Bitmap
import android.os.SystemClock
import android.view.MotionEvent
import android.view.ViewConfiguration
import android.widget.FrameLayout
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
class WhitespaceSelectionTest {
    @Test fun blankLongPressOwnsItsDragWhileQuickSwipesStillTurn(){
        val instrumentation=InstrumentationRegistry.getInstrumentation()
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        var activity:MainActivity?=null;var bitmap:Bitmap?=null
        val selects=AtomicInteger();val turns=AtomicInteger();val intercepted=AtomicInteger();val moving=AtomicBoolean()
        lateinit var view:ReaderView
        var created=false
        try {
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(8000) as MainActivity
            instrumentation.runOnMainSync {
                val parent=object:FrameLayout(activity!!){override fun requestDisallowInterceptTouchEvent(disallow:Boolean){if(disallow)intercepted.incrementAndGet();super.requestDisallowInterceptTouchEvent(disallow)}}
                view=ReaderView(activity!!);created=true;parent.addView(view,FrameLayout.LayoutParams(-1,-1));activity!!.setContentView(parent)
            }
            instrumentation.waitForIdleSync();SystemClock.sleep(200)
            instrumentation.runOnMainSync {
                bitmap=Bitmap.createBitmap(view.width,view.height,Bitmap.Config.ARGB_8888)
                val density=view.resources.displayMetrics.density
                val info=PageInfo(section=0u,sectionCount=2u,page=0u,pageCount=1u,progress=0.0,regions=emptyList(),marks=emptyList(),focusBounds=emptyList(),focusFadeColor=0u,focusHighlight=false,isScroll=true,
                    scrollDistance=0.0,scrollContext=123uL,rasterTop=0f,rasterHeight=view.height/density,previousFocus=null,nextFocus=FocusTarget(48f,emptyList()),controls=emptyList(),activeTocId="one",activeTocPath=emptyList(),focusIndex=0u,focusCount=2u,canGoBack=false)
                view.present(ReaderFrame(bitmap!!,info),null)
                view.onSelect={x,_,_,_->assertEquals(32f,x,.1f);selects.incrementAndGet()}
                view.onTurn={turns.incrementAndGet()};view.onMotion={moving.set(it)}
            }
            val density=view.resources.displayMetrics.density
            var down=SystemClock.uptimeMillis()
            fun event(action:Int,x:Float,y:Float){MotionEvent.obtain(down,SystemClock.uptimeMillis(),action,x*density,y*density,0).also {instrumentation.sendPointerSync(it);it.recycle()}}
            event(MotionEvent.ACTION_DOWN,32f,200f)
            SystemClock.sleep(ViewConfiguration.getLongPressTimeout()+150L)
            assertTrue("A margin long press starts selection",selects.get()>0)
            assertTrue("A note/list cannot intercept an active selection drag",intercepted.get()>0)
            assertTrue("Toolbar and AI updates wait while selecting",moving.get())
            event(MotionEvent.ACTION_MOVE,view.width/density-1f,250f)
            event(MotionEvent.ACTION_UP,view.width/density-1f,250f)
            assertTrue(selects.get()>=3);assertEquals(0,turns.get());assertFalse(moving.get())
            val before=selects.get();down=SystemClock.uptimeMillis()
            event(MotionEvent.ACTION_DOWN,100f,340f);SystemClock.sleep(40)
            event(MotionEvent.ACTION_MOVE,100f,200f);SystemClock.sleep(40)
            event(MotionEvent.ACTION_UP,100f,200f)
            assertEquals("A quick drag remains reading navigation",1,turns.get())
            assertEquals(before,selects.get())
        }finally {
            instrumentation.runOnMainSync {if(created)view.cancelPendingNavigation();activity?.finish()}
            instrumentation.removeMonitor(monitor);bitmap?.recycle()
        }
    }
}

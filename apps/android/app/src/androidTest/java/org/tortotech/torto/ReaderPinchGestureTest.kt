package org.tortotech.torto

import android.graphics.Bitmap
import android.os.SystemClock
import android.view.MotionEvent
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.tortotech.torto.core.PageInfo
import org.tortotech.torto.core.ReadingControl

@RunWith(AndroidJUnit4::class)
class ReaderPinchGestureTest {
    @Test fun spreadPinchAndMultitouchNeverLeakIntoNavigationOrSelection(){
        val instrumentation=InstrumentationRegistry.getInstrumentation()
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        var activity:MainActivity?=null;var bitmap:Bitmap?=null
        try {
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity;assertNotNull(activity)
            lateinit var view:ReaderView
            instrumentation.runOnMainSync {view=ReaderView(activity!!);activity!!.setContentView(view)}
            instrumentation.waitForIdleSync();SystemClock.sleep(200)
            instrumentation.runOnMainSync {
                val density=view.resources.displayMetrics.density;val height=view.height/density
                bitmap=Bitmap.createBitmap(view.width,view.height,Bitmap.Config.ARGB_8888)
                val info=PageInfo(section=0u,sectionCount=2u,page=0u,pageCount=1u,progress=0.0,regions=emptyList(),marks=emptyList(),
                    focusBounds=emptyList(),focusFadeColor=0u,focusHighlight=false,isScroll=true,scrollDistance=0.0,scrollContext=1uL,
                    rasterTop=0f,rasterHeight=height,previousFocus=null,nextFocus=null,
                    controls=listOf(ReadingControl("sentences","focused-source",false)),activeTocId=null,activeTocPath=emptyList(),focusIndex=0u,focusCount=1u,canGoBack=false)
                view.present(ReaderFrame(bitmap!!,info,1uL),null)
                val splits=mutableListOf<Boolean>();var navigation=0;var taps=0;var selects=0;var moving=false
                view.onSentenceGesture={split,source->assertEquals("focused-source",source);splits.add(split)}
                view.onTurn={navigation++};view.onUnitTurn={navigation++};view.onTap={_,_->taps++};view.onSelect={_,_,_,_->selects++};view.onMotion={moving=it}
                var down=SystemClock.uptimeMillis();var elapsed=0L
                fun event(action:Int,count:Int,span:Float,yOffset:Float=0f){
                    elapsed+=25
                    val properties=Array(count){i->MotionEvent.PointerProperties().apply {id=i;toolType=MotionEvent.TOOL_TYPE_FINGER}}
                    val coords=Array(count){i->MotionEvent.PointerCoords().apply {x=view.width*.5f+(i-.5f)*span*density;y=view.height*.5f+yOffset*density;pressure=1f;size=1f}}
                    MotionEvent.obtain(down,down+elapsed,action,count,properties,coords,0,0,1f,1f,0,0,android.view.InputDevice.SOURCE_TOUCHSCREEN,0).also {view.onTouchEvent(it);it.recycle()}
                }
                fun gesture(start:Float,end:Float,third:Boolean=false,cancel:Boolean=false,dragFirst:Boolean=false){
                    down=SystemClock.uptimeMillis();elapsed=0
                    event(0,1,start)
                    if(dragFirst)event(2,1,start,35f)
                    event(5 or (1 shl 8),2,start)
                    event(2,2,end)
                    if(third){event(5 or (2 shl 8),3,end);event(6 or (2 shl 8),3,end)}
                    event(6 or (1 shl 8),2,end)
                    event(if(cancel)3 else 1,1,end)
                    assertFalse("Every completed gesture releases motion",moving)
                }
                gesture(100f,160f,dragFirst=true);assertEquals(listOf(true),splits)
                gesture(160f,90f);assertEquals(listOf(true,false),splits)
                gesture(100f,105f);gesture(100f,160f,third=true);gesture(100f,160f,cancel=true)
                assertEquals(listOf(true,false),splits)
                assertEquals("Multitouch must not turn paragraphs or chapters",0,navigation)
                assertEquals("Neither remaining finger may become a tap",0,taps);assertEquals(0,selects)
                // Regular single-finger input is immediately usable afterwards.
                event(0,1,100f);event(1,1,100f);assertEquals(1,taps)
            }
        }finally {instrumentation.runOnMainSync {activity?.finish()};instrumentation.removeMonitor(monitor);bitmap?.recycle()}
    }
}

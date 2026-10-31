package org.tortotech.torto

import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Rect
import android.os.Bundle
import android.view.GestureDetector
import android.view.MotionEvent
import android.view.View
import android.view.accessibility.AccessibilityEvent
import android.widget.OverScroller
import androidx.core.view.ViewCompat
import androidx.core.view.accessibility.AccessibilityNodeInfoCompat
import androidx.customview.widget.ExploreByTouchHelper
import org.tortotech.torto.core.SelectedText
import kotlin.math.abs
import kotlin.math.hypot

/** Retained native page; gesture animation stops when its finite fling or handle drag ends. */
class ReaderView(context: Context) : View(context) {
    var onSize: ((Int, Int, Float) -> Unit)? = null
    var onTap: ((Float, Float) -> Unit)? = null
    var onTurn: ((Boolean) -> Unit)? = null
    var onSelect: ((Float, Float, Float, Float) -> Unit)? = null
    var onScroll: ((Float) -> Unit)? = null
    var onHandleStart: ((Boolean) -> Unit)? = null
    var onHandleMove: ((Float, Float) -> Unit)? = null
    var onHandleEnd: (() -> Unit)? = null
    private val paint = Paint(Paint.ANTI_ALIAS_FLAG or Paint.FILTER_BITMAP_FLAG)
    private val highlight = Paint().apply { color = 0x55389875 }
    private val handlePaint = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = 0xFF398975.toInt() }
    private val focusShade = Paint()
    private val savedHighlight = Paint().apply { color = 0x55D8B86A }
    private var frame: ReaderFrame? = null
    private var selected: SelectedText? = null
    private val density get() = resources.displayMetrics.density
    private var anchorX = 0f; private var anchorY = 0f
    private var selecting = false
    private var draggingHandle = false
    private var pointerX = 0f; private var pointerY = 0f
    private var pendingScroll = 0.0
    private val scroller = OverScroller(context)
    private var flingY = 0
    private val pendingOffset get() = if (frame?.info?.isScroll == true) (pendingScroll * density).toFloat() else 0f
    private val edgeScroll = object : Runnable {
        override fun run() {
            if (!draggingHandle || frame?.info?.isScroll != true || !isShown) return
            val edge = 56 * density
            val delta = when { pointerY < edge -> -12f; pointerY > height-edge -> 12f; else -> return }
            scroll(delta)
            onHandleMove?.invoke(pointerX/density, pointerY/density)
            postDelayed(this, 40)
        }
    }
    private val accessibility = object : ExploreByTouchHelper(this) {
        override fun onPopulateNodeForHost(node: AccessibilityNodeInfoCompat) {
            super.onPopulateNodeForHost(node)
            node.isScrollable = true
            node.addAction(AccessibilityNodeInfoCompat.AccessibilityActionCompat.ACTION_SCROLL_FORWARD)
            node.addAction(AccessibilityNodeInfoCompat.AccessibilityActionCompat.ACTION_SCROLL_BACKWARD)
        }
        override fun getVirtualViewAt(x: Float, y: Float): Int {
            val regions = frame?.info?.regions ?: return INVALID_ID
            return regions.indexOfFirst { r -> r.bounds.any { x/density >= it.x && x/density <= it.x+it.width && (y+pendingOffset)/density >= it.y && (y+pendingOffset)/density <= it.y+it.height } }.takeIf { it >= 0 } ?: INVALID_ID
        }
        override fun getVisibleVirtualViews(ids: MutableList<Int>) {
            frame?.info?.regions?.forEachIndexed { i,r -> if(r.bounds.any {it.y*density-pendingOffset < height && (it.y+it.height)*density-pendingOffset > 0}) ids.add(i) }
        }
        override fun onPopulateNodeForVirtualView(id: Int, node: AccessibilityNodeInfoCompat) {
            val region = frame?.info?.regions?.getOrNull(id)
            node.text = region?.text ?: " "
            node.className = "android.widget.TextView"
            val bounds = Rect()
            region?.bounds?.forEach { r -> bounds.union((r.x*density).toInt(), (r.y*density-pendingOffset).toInt(), ((r.x+r.width)*density).toInt(), ((r.y+r.height)*density-pendingOffset).toInt()) }
            if (bounds.isEmpty) bounds.set(0,0,1,1)
            node.setBoundsInParent(bounds)
        }
        override fun onPerformActionForVirtualView(id: Int, action: Int, args: Bundle?): Boolean = false
    }
    private fun scroll(delta:Float) { pendingScroll += delta; onScroll?.invoke(delta); invalidate() }
    private val gestures = GestureDetector(context, object : GestureDetector.SimpleOnGestureListener() {
        override fun onDown(e: MotionEvent) = true
        override fun onSingleTapUp(e: MotionEvent): Boolean { performClick();onTap?.invoke(e.x/density,e.y/density);return true }
        override fun onScroll(a:MotionEvent?,b:MotionEvent,dx:Float,dy:Float):Boolean {
            if(!selecting && frame?.info?.isScroll==true && a!=null && abs(b.y-a.y)>abs(b.x-a.x)){scroll(dy/density);return true};return false
        }
        override fun onFling(a: MotionEvent?, b: MotionEvent, vx: Float, vy: Float): Boolean {
            if(selecting || a==null)return false
            if(abs(b.x-a.x)>40*density && abs(vx)>abs(vy)){onTurn?.invoke(vx<0);return true}
            if(frame?.info?.isScroll==true && abs(vy)>abs(vx)) {flingY=0;scroller.fling(0,0,0,(-vy).toInt(),0,0,-height*6,height*6);postInvalidateOnAnimation();return true}
            return false
        }
        override fun onLongPress(e: MotionEvent) { selecting=true;anchorX=e.x/density;anchorY=e.y/density;onSelect?.invoke(anchorX,anchorY,anchorX,anchorY) }
    })
    init { isFocusable=true;ViewCompat.setAccessibilityDelegate(this,accessibility) }
    fun present(next:ReaderFrame?,selection:SelectedText?) {
        val frameChanged=frame!==next
        val changed=frameChanged||selected!=selection
        if(frameChanged){
            val old=frame
            if(old?.info?.isScroll==true && next?.info?.isScroll==true){
                val acknowledged=next.info.scrollDistance-old.info.scrollDistance
                if(acknowledged!=0.0)pendingScroll=if(acknowledged*pendingScroll>0.0 && abs(acknowledged)<=abs(pendingScroll)+0.01)pendingScroll-acknowledged else 0.0
            }else pendingScroll=0.0
            frame=next;accessibility.invalidateRoot()
        }
        selected=selection
        if(changed)invalidate()
    }
    override fun computeScroll() {if(scroller.computeScrollOffset()){val y=scroller.currY;scroll((y-flingY)/density);flingY=y;postInvalidateOnAnimation()}}
    override fun onSizeChanged(w:Int,h:Int,oldw:Int,oldh:Int) {onSize?.invoke(w,h,density)}
    private fun handlePoints():List<Pair<Float,Float>> {
        val bounds=selected?.bounds.orEmpty();if(bounds.isEmpty())return emptyList()
        return listOf(bounds.first().let {it.x*density to (it.y+it.height)*density-pendingOffset},bounds.last().let {(it.x+it.width)*density to (it.y+it.height)*density-pendingOffset})
    }
    override fun onDraw(canvas:Canvas) {
        super.onDraw(canvas)
        canvas.save();canvas.translate(0f,-pendingOffset)
        frame?.bitmap?.let {canvas.drawBitmap(it,null,Rect(0,0,width,height),paint)}
        frame?.info?.marks?.forEach {canvas.drawRect(it.x*density,it.y*density,(it.x+it.width)*density,(it.y+it.height)*density,savedHighlight)}
        frame?.info?.focusBounds?.takeIf {it.isNotEmpty()}?.let {bounds->
            if(frame!!.info.focusHighlight){bounds.forEach {r->canvas.drawRect((r.x-4)*density,(r.y-2)*density,(r.x+r.width+4)*density,(r.y+r.height+2)*density,highlight)}}
            else{focusShade.color=frame!!.info.focusFadeColor.toInt();canvas.save()
                bounds.forEach {r->canvas.clipOutRect(((r.x-8)*density).coerceAtLeast(0f),(r.y-2)*density,((r.x+r.width+8)*density).coerceAtMost(width.toFloat()),(r.y+r.height+2)*density)}
                canvas.drawRect(0f,pendingOffset,width.toFloat(),height+pendingOffset,focusShade);canvas.restore()}
        }
        selected?.bounds?.forEach {canvas.drawRect(it.x*density,it.y*density,(it.x+it.width)*density,(it.y+it.height)*density,highlight)}
        canvas.restore()
        handlePoints().forEach {(x,y)->if(y>=0 && y<=height)canvas.drawCircle(x,y+6*density,7*density,handlePaint)}
    }
    override fun onTouchEvent(event:MotionEvent):Boolean {
        if(event.actionMasked==MotionEvent.ACTION_DOWN){
            scroller.abortAnimation();selecting=false
            val hit=handlePoints().indexOfFirst {(x,y)->hypot(event.x-x,event.y-y-6*density)<26*density}
            if(hit>=0){draggingHandle=true;pointerX=event.x;pointerY=event.y;onHandleStart?.invoke(hit==0);parent?.requestDisallowInterceptTouchEvent(true);return true}
        }
        if(draggingHandle){
            pointerX=event.x;pointerY=event.y-8*density
            if(event.actionMasked==MotionEvent.ACTION_MOVE){onHandleMove?.invoke(pointerX/density,pointerY/density);removeCallbacks(edgeScroll);post(edgeScroll)}
            if(event.actionMasked==MotionEvent.ACTION_UP || event.actionMasked==MotionEvent.ACTION_CANCEL){if(event.actionMasked==MotionEvent.ACTION_UP)onHandleMove?.invoke(pointerX/density,pointerY/density);draggingHandle=false;removeCallbacks(edgeScroll);onHandleEnd?.invoke();parent?.requestDisallowInterceptTouchEvent(false)}
            return true
        }
        gestures.onTouchEvent(event)
        if(selecting && (event.actionMasked==MotionEvent.ACTION_MOVE || event.actionMasked==MotionEvent.ACTION_UP))onSelect?.invoke(anchorX,anchorY,event.x/density,event.y/density)
        if(event.actionMasked==MotionEvent.ACTION_UP || event.actionMasked==MotionEvent.ACTION_CANCEL)selecting=false
        return true
    }
    private fun stopMotion(){scroller.abortAnimation();draggingHandle=false;selecting=false;removeCallbacks(edgeScroll);onHandleEnd?.invoke()}
    override fun onDetachedFromWindow(){stopMotion();super.onDetachedFromWindow()}
    override fun onWindowFocusChanged(hasWindowFocus:Boolean){super.onWindowFocusChanged(hasWindowFocus);if(!hasWindowFocus)stopMotion()}
    override fun performClick():Boolean {super.performClick();return true}
    override fun dispatchHoverEvent(event:MotionEvent)=accessibility.dispatchHoverEvent(event)||super.dispatchHoverEvent(event)
    override fun performAccessibilityAction(action:Int,arguments:Bundle?):Boolean {
        if(action==AccessibilityNodeInfoCompat.ACTION_SCROLL_FORWARD || action==AccessibilityNodeInfoCompat.ACTION_SCROLL_BACKWARD){onTurn?.invoke(action==AccessibilityNodeInfoCompat.ACTION_SCROLL_FORWARD);sendAccessibilityEvent(AccessibilityEvent.TYPE_VIEW_SCROLLED);return true}
        return super.performAccessibilityAction(action,arguments)
    }
}

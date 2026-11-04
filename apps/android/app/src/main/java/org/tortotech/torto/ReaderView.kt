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
import android.animation.Animator
import android.animation.AnimatorListenerAdapter
import android.animation.ValueAnimator
import android.graphics.RectF
import android.view.VelocityTracker
import org.tortotech.torto.core.focusMotionDuration
import androidx.core.view.ViewCompat
import androidx.core.view.accessibility.AccessibilityNodeInfoCompat
import androidx.customview.widget.ExploreByTouchHelper
import org.tortotech.torto.core.SelectedText
import kotlin.math.abs
import kotlin.math.hypot
import kotlin.math.roundToInt

/** Touch previews translate retained content; committed focus stops update native state. */
class ReaderView(context: Context) : View(context) {
    var onMotion: ((Boolean) -> Unit)? = null
    var onReadingGesture: (() -> Unit)? = null
    private var reportedGesture=false
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
    private var displayPosition=0.0
    private var previousFrame:ReaderFrame?=null
    private var focusPreview:Pair<Double,List<org.tortotech.torto.core.Rect>>?=null
    private var motion:ValueAnimator?=null
    private var velocity:VelocityTracker?=null
    private var draggingFocus=false
    private var navigationPending=false
    private var blockedGesture=false
    private var reportedMotion=false
    private var dragStart=0.0
    private var dragDistance=0f
    private val pendingOffset get()=if(frame?.info?.isScroll==true)((displayPosition-(frame?.info?.scrollDistance ?: 0.0))*density).toFloat() else 0f
    private val edgeScroll = object : Runnable {
        override fun run() {
            if (!draggingHandle || frame?.info?.isScroll != true || !isShown) return
            val edge = 56 * density
            val delta = when { pointerY < edge -> -12f; pointerY > height-edge -> 12f; else -> return }
            onScroll?.invoke(delta)
            onHandleMove?.invoke(pointerX/density, (pointerY+pendingOffset)/density)
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
    private fun reportMotion() {
        val active = draggingFocus || draggingHandle || motion != null || navigationPending
        if (active != reportedMotion) { reportedMotion = active; onMotion?.invoke(active) }
    }
    private fun cancelAnimation() { val old=motion;motion=null;old?.cancel() }
    private fun setPosition(value:Double) {
        val current=frame ?: return
        if(!current.info.isScroll){displayPosition=current.info.scrollDistance;return}
        val viewport=height/density
        fun range(f:ReaderFrame):Pair<Double,Double> {
            val start=f.info.scrollDistance+f.info.rasterTop
            return start to (start+f.info.rasterHeight-viewport).coerceAtLeast(start)
        }
        var (low,high)=range(current)
        previousFrame?.let {old->val (a,b)=range(old);low=minOf(low,a);high=maxOf(high,b)}
        displayPosition=value.coerceIn(low,high)
        invalidate()
    }
    private fun animateTo(target:Double) {
        cancelAnimation()
        if(abs(target-displayPosition)<0.25 || !isShown || !hasWindowFocus()) {
            setPosition(target);previousFrame=null;reportMotion();return
        }
        val start=displayPosition
        val animator=ValueAnimator.ofFloat(0f,1f)
        motion=animator
        animator.duration=focusMotionDuration((target-start).toFloat()).toLong()
        animator.interpolator=android.animation.TimeInterpolator {t->t*t*(3f-2f*t)}
        animator.addUpdateListener { setPosition(start+(target-start)*(it.animatedValue as Float)) }
        animator.addListener(object:AnimatorListenerAdapter(){override fun onAnimationEnd(animation:Animator){
            if(motion!==animator)return
            motion=null
            if(!navigationPending)previousFrame=null
            reportMotion()
        }})
        reportMotion();animator.start()
    }
    private fun stepFocus(forward:Boolean) {
        val current=frame ?: return
        if(!current.info.isScroll){onTurn?.invoke(forward);return}
        if(navigationPending||!isEnabled)return
        val target=if(forward)current.info.nextFocus else current.info.previousFocus
        if(target==null){animateTo(current.info.scrollDistance);return}
        navigationPending=true
        focusPreview=current.info.scrollDistance to target.bounds
        onTurn?.invoke(forward)
        animateTo(current.info.scrollDistance+target.delta)
        reportMotion()
    }
    private fun finishDrag(velocity:Float,cancelled:Boolean) {
        if(!draggingFocus)return
        draggingFocus=false
        val commit=!cancelled&&(abs(dragDistance)>=36f||abs(velocity)>=500f)
        if(commit)stepFocus(if(abs(velocity)>=500f)velocity>0 else dragDistance>0)
        else animateTo(frame?.info?.scrollDistance ?: displayPosition)
        reportMotion()
    }
    private val gestures=GestureDetector(context,object:GestureDetector.SimpleOnGestureListener(){
        override fun onDown(e:MotionEvent)=true
        override fun onSingleTapUp(e:MotionEvent):Boolean {
            performClick();onTap?.invoke(e.x/density,(e.y+pendingOffset)/density);return true
        }
        override fun onScroll(a:MotionEvent?,b:MotionEvent,dx:Float,dy:Float):Boolean {
            val current=frame ?: return false
            if(selecting||a==null)return false
            if(!reportedGesture&&(current.info.isScroll||abs(b.x-a.x)>abs(b.y-a.y))){reportedGesture=true;onReadingGesture?.invoke()}
            if(!current.info.isScroll||abs(b.y-a.y)<=abs(b.x-a.x))return false
            draggingFocus=true;dragDistance+=dy/density
            val low=minOf(dragStart,current.info.scrollDistance+(current.info.previousFocus?.delta ?: 0f))
            val high=maxOf(dragStart,current.info.scrollDistance+(current.info.nextFocus?.delta ?: 0f))
            setPosition((dragStart+dragDistance).coerceIn(low,high));reportMotion();return true
        }
        override fun onFling(a:MotionEvent?,b:MotionEvent,vx:Float,vy:Float):Boolean {
            if(selecting||draggingFocus||a==null)return false
            if(abs(b.x-a.x)>40*density&&abs(vx)>abs(vy)){stepFocus(vx<0);return true}
            return false
        }
        override fun onLongPress(e:MotionEvent){
            selecting=true;anchorX=e.x/density;anchorY=(e.y+pendingOffset)/density
            onSelect?.invoke(anchorX,anchorY,anchorX,anchorY)
        }
    })
    init {isFocusable=true;ViewCompat.setAccessibilityDelegate(this,accessibility)}
    fun present(next:ReaderFrame?,selection:SelectedText?){
        val old=frame
        if(old!==next){
            val continuous=old?.info?.isScroll==true&&next?.info?.isScroll==true&&old.info.scrollContext==next.info.scrollContext
            val pending=navigationPending
            if(!continuous){
                stopMotion();displayPosition=next?.info?.scrollDistance ?: 0.0
                previousFrame=null;frame=next
            }else{
                if(old.rasterId!=next.rasterId&&(motion!=null||pending||draggingFocus))previousFrame=old
                frame=next;navigationPending=false
                if(pending)focusPreview=null
                if(!draggingFocus&&!draggingHandle){
                    if(motion==null||pending)animateTo(next.info.scrollDistance)
                }
            }
            accessibility.invalidateRoot();reportMotion()
        }
        val changed=old!==next||selected!=selection
        selected=selection
        if(changed)invalidate()
    }
    override fun onSizeChanged(w:Int,h:Int,oldw:Int,oldh:Int){stopMotion();onSize?.invoke(w,h,density)}
    private fun handlePoints():List<Pair<Float,Float>> {
        val bounds=selected?.bounds.orEmpty();if(bounds.isEmpty())return emptyList()
        return listOf(bounds.first().let {it.x*density to (it.y+it.height)*density-pendingOffset},bounds.last().let {(it.x+it.width)*density to (it.y+it.height)*density-pendingOffset})
    }
    private fun drawRaster(canvas:Canvas,f:ReaderFrame){
        // Keep retained text on physical pixels, including after a fractional stop.
        val top=((f.info.rasterTop+f.info.scrollDistance-displayPosition).toFloat()*density).roundToInt().toFloat()
        canvas.drawBitmap(f.bitmap,null,RectF(0f,top,f.bitmap.width.toFloat(),top+f.bitmap.height),paint)
    }
    override fun onDraw(canvas:Canvas){
        super.onDraw(canvas)
        val current=frame ?: return
        val clipped=canvas.save()
        canvas.clipRect(0,0,width,height)
        if(current.info.isScroll){canvas.drawColor((current.info.focusFadeColor.toInt() and 0xFFFFFF) or 0xFF000000.toInt());previousFrame?.let {drawRaster(canvas,it)};drawRaster(canvas,current)}
        else canvas.drawBitmap(current.bitmap,null,Rect(0,0,width,height),paint)
        canvas.save();canvas.translate(0f,-pendingOffset)
        current.info.marks.forEach {canvas.drawRect(it.x*density,it.y*density,(it.x+it.width)*density,(it.y+it.height)*density,savedHighlight)}
        val focusShift=focusPreview?.let {(it.first-current.info.scrollDistance).toFloat()} ?: 0f
        (focusPreview?.second ?: current.info.focusBounds).takeIf {it.isNotEmpty()}?.let {bounds->
            if(current.info.focusHighlight){bounds.forEach {r->canvas.drawRect((r.x-4)*density,(r.y+focusShift-2)*density,(r.x+r.width+4)*density,(r.y+focusShift+r.height+2)*density,highlight)}}
            else{focusShade.color=current.info.focusFadeColor.toInt();canvas.save()
                bounds.forEach {r->canvas.clipOutRect(((r.x-8)*density).coerceAtLeast(0f),(r.y+focusShift-2)*density,((r.x+r.width+8)*density).coerceAtMost(width.toFloat()),(r.y+focusShift+r.height+2)*density)}
                canvas.drawRect(0f,pendingOffset,width.toFloat(),height+pendingOffset,focusShade);canvas.restore()}
        }
        selected?.bounds?.forEach {canvas.drawRect(it.x*density,it.y*density,(it.x+it.width)*density,(it.y+it.height)*density,highlight)}
        canvas.restore()
        handlePoints().forEach {(x,y)->if(y>=0&&y<=height)canvas.drawCircle(x,y+6*density,7*density,handlePaint)}
        canvas.restoreToCount(clipped)
    }
    override fun onTouchEvent(event:MotionEvent):Boolean {
        if(event.actionMasked==MotionEvent.ACTION_DOWN){
            blockedGesture=navigationPending||!isEnabled
            if(blockedGesture)return true
            cancelAnimation();selecting=false;draggingFocus=false;reportedGesture=false;dragDistance=0f;dragStart=displayPosition
            velocity?.recycle();velocity=VelocityTracker.obtain()
            val hit=handlePoints().indexOfFirst {(x,y)->hypot(event.x-x,event.y-y-6*density)<26*density}
            if(hit>=0){draggingHandle=true;pointerX=event.x;pointerY=event.y;onHandleStart?.invoke(hit==0);reportMotion();parent?.requestDisallowInterceptTouchEvent(true)}
        }
        if(blockedGesture)return true
        velocity?.addMovement(event)
        if(draggingHandle){
            pointerX=event.x;pointerY=event.y-8*density
            if(event.actionMasked==MotionEvent.ACTION_MOVE){onHandleMove?.invoke(pointerX/density,(pointerY+pendingOffset)/density);removeCallbacks(edgeScroll);post(edgeScroll)}
            if(event.actionMasked==MotionEvent.ACTION_UP||event.actionMasked==MotionEvent.ACTION_CANCEL){
                if(event.actionMasked==MotionEvent.ACTION_UP)onHandleMove?.invoke(pointerX/density,(pointerY+pendingOffset)/density)
                draggingHandle=false;removeCallbacks(edgeScroll);onHandleEnd?.invoke();reportMotion();parent?.requestDisallowInterceptTouchEvent(false)
                velocity?.recycle();velocity=null
            }
            return true
        }
        gestures.onTouchEvent(event)
        if(selecting&&(event.actionMasked==MotionEvent.ACTION_MOVE||event.actionMasked==MotionEvent.ACTION_UP))onSelect?.invoke(anchorX,anchorY,event.x/density,(event.y+pendingOffset)/density)
        if(event.actionMasked==MotionEvent.ACTION_UP||event.actionMasked==MotionEvent.ACTION_CANCEL){
            velocity?.computeCurrentVelocity(1000)
            finishDrag(-(velocity?.yVelocity ?: 0f)/density,event.actionMasked==MotionEvent.ACTION_CANCEL)
            velocity?.recycle();velocity=null;selecting=false;reportMotion()
        }
        return true
    }
    private fun stopMotion(){
        val hadHandle=draggingHandle
        cancelAnimation();navigationPending=false;blockedGesture=false;draggingFocus=false;draggingHandle=false;selecting=false
        previousFrame=null;focusPreview=null;displayPosition=frame?.info?.scrollDistance ?: 0.0
        velocity?.recycle();velocity=null;removeCallbacks(edgeScroll);if(hadHandle)onHandleEnd?.invoke();reportMotion()
    }
    override fun onDetachedFromWindow(){stopMotion();super.onDetachedFromWindow()}
    override fun onWindowFocusChanged(hasWindowFocus:Boolean){super.onWindowFocusChanged(hasWindowFocus);if(!hasWindowFocus)stopMotion()}
    override fun performClick():Boolean {super.performClick();return true}
    override fun dispatchHoverEvent(event:MotionEvent)=accessibility.dispatchHoverEvent(event)||super.dispatchHoverEvent(event)
    override fun performAccessibilityAction(action:Int,arguments:Bundle?):Boolean {
        if(action==AccessibilityNodeInfoCompat.ACTION_SCROLL_FORWARD || action==AccessibilityNodeInfoCompat.ACTION_SCROLL_BACKWARD){stepFocus(action==AccessibilityNodeInfoCompat.ACTION_SCROLL_FORWARD);sendAccessibilityEvent(AccessibilityEvent.TYPE_VIEW_SCROLLED);return true}
        return super.performAccessibilityAction(action,arguments)
    }
}

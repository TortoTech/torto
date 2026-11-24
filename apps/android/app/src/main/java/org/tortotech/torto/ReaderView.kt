package org.tortotech.torto

import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Path
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
import android.view.ViewConfiguration
import androidx.compose.animation.core.getVelocityFromNanos
import org.tortotech.torto.core.pageTurnCommit
import org.tortotech.torto.core.pageTurnResistedOffset
import androidx.core.view.ViewCompat
import androidx.core.view.accessibility.AccessibilityNodeInfoCompat
import androidx.customview.widget.ExploreByTouchHelper
import org.tortotech.torto.core.SelectedText
import kotlin.math.abs
import kotlin.math.hypot
import kotlin.math.roundToInt

/** Touch previews translate retained content; committed focus stops update native state. */
class ReaderView(context: Context) : View(context) {
    var animateOpening=false
    var embeddedInScrollContainer=false
    var onMotion: ((Boolean) -> Unit)? = null
    var onContentLoading: ((Boolean) -> Unit)? = null
    var onReadingGesture: (() -> Unit)? = null
    private var reportedGesture=false
    var onSize: ((Int, Int, Float) -> Unit)? = null
    var onTap: ((Float, Float) -> Unit)? = null
    var onTurn: ((Boolean) -> Unit)? = null
    var onUnitTurn: ((Boolean) -> Unit)? = null
    var onSelect: ((Float, Float, Float, Float) -> Unit)? = null
    var onScroll: ((Float) -> Unit)? = null
    var onSentenceGesture: ((Boolean,String) -> Unit)? = null
    var onFocusHighlightGesture: ((Boolean) -> Unit)? = null
    var onFocusNoteGesture: ((Boolean) -> Unit)? = null
    var onFocusChatGesture: (() -> Unit)? = null
    var onFocusBadgeTap: ((String) -> Unit)? = null
    var onInlineOffsetChange: ((Float) -> Unit)? = null
    private var inlineGaps: List<Pair<Float, Float>> = emptyList()
    private var inlineShiftY = 0f
    private var lastReportedInlineOffset = Float.NaN
    private val badgeBgPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = 0xE0448967.toInt() }
    private val badgeTextPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        color = 0xFFFFFFFF.toInt()
        textAlign = Paint.Align.CENTER
        isFakeBoldText = true
    }
    private var pinchConsuming=false
    private var focusMultiEligible=false
    private var multiMaxPointers=0
    private var multiStartX=0f
    private var multiStartY=0f
    private var multiEndX=0f
    private var multiEndY=0f
    private var multiStartTime=0L
    private var multiMoved=false
    private var pinchSource:String?=null
    var onHandleStart: ((Boolean) -> Unit)? = null
    var onHandleMove: ((Float, Float) -> Unit)? = null
    var onHandleEnd: (() -> Unit)? = null
    private val paint = Paint(Paint.ANTI_ALIAS_FLAG or Paint.FILTER_BITMAP_FLAG)
    private val highlight = Paint().apply { color = 0x55448967 }
    private val handlePaint = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = 0xFF448967.toInt() }
    private val focusShade = Paint()
    private val savedHighlight = Paint().apply { color = 0x55D8B86A }
    private val referencePaint=Paint(Paint.ANTI_ALIAS_FLAG)
    private var referenceIndicator:org.tortotech.torto.core.Rect?=null
    private var frame: ReaderFrame? = null
    private var selected: SelectedText? = null
    private val selectionPath = Path()
    private val highlightPath = Path()
    private fun drawMergedRects(canvas: Canvas, rects: List<org.tortotech.torto.core.Rect>, paint: Paint, padX: Float = 0f, padY: Float = 0f, shiftY: Float = 0f) {
        if (rects.isEmpty()) return
        highlightPath.rewind()
        rects.forEach { r ->
            highlightPath.addRect((r.x - padX) * density, (r.y + shiftY - padY) * density, (r.x + r.width + padX) * density, (r.y + shiftY + r.height + padY) * density, Path.Direction.CW)
        }
        canvas.drawPath(highlightPath, paint)
    }
    private val density get() = resources.displayMetrics.density
    private var anchorX = 0f; private var anchorY = 0f
    private var selecting = false
    private var draggingHandle = false
    private var pointerX = 0f; private var pointerY = 0f
    private var displayPosition=0.0
    private var previousFrame:ReaderFrame?=null
    private var focusPreview:Pair<Double,List<org.tortotech.torto.core.Rect>>?=null
    private var motion:ValueAnimator?=null
    private var motionTarget:Double?=null
    private var motionClipped=false
    private var motionVelocity=0f
    private var pendingFocusTarget:Double?=null
    private var deferredFocusGesture=false
    private var queuedFocusRelease=false
    private var queuedFocusTurn:Boolean?=null
    private var queuedFocusVelocity=0f
    private var queuedUnitTurn:Boolean?=null
    private var gestureAxis=0
    private var releaseVelocityX=0f
    private var releaseVelocityY=0f
    private var lastMoveTime=0L
    private val touchSlop=ViewConfiguration.get(context).scaledTouchSlop.toFloat()
    private val maxVelocity=ViewConfiguration.get(context).scaledMaximumFlingVelocity.toFloat()
    private var velocity:VelocityTracker?=null
    private var draggingFocus=false
    private var draggingUnit=false
    private var unitDistance=0f
    private var draggingPage=false
    private var pageDirection=0
    private var pageDistance=0f
    private var pageOffset=0f
    private var pageIsUnit=false
    private var turnNeighbour:ReaderFrame?=null
    private var previousPage:ReaderFrame?=null
    private var nextPage:ReaderFrame?=null
    private var navigationPending=false
    private var boundaryTransition=false
    private var boundaryOffset=0f
    private var contentOpacity=1f
    private var contentFade:ValueAnimator?=null
    private var fadingBoundaryOut=false
    private data class Presentation(val frame:ReaderFrame?,val selection:SelectedText?,val previous:ReaderFrame?,val next:ReaderFrame?,val completion:Long)
    private var pendingPresentation:Presentation?=null
    private var contentLoading=false
    private var lastTurnCompletion=0L
    private var blockedGesture=false
    private var reportedMotion=false
    private var dragStart=0.0
    private var dragDistance=0f
    private val pendingOffset get()=if(frame?.info?.isScroll==true)((displayPosition-(frame?.info?.scrollDistance ?: 0.0))*density).roundToInt().toFloat() else 0f
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
            if(embeddedInScrollContainer)return
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
        val active = pinchConsuming || selecting || draggingFocus || draggingUnit || draggingPage || draggingHandle || motion != null || contentFade != null || navigationPending
        if (active != reportedMotion) { reportedMotion = active; onMotion?.invoke(active) }
    }
    private fun reportContentLoading(active:Boolean){
        if(contentLoading!=active){contentLoading=active;onContentLoading?.invoke(active)}
    }
    private fun animateBoundaryTurn(forward:Boolean){
        cancelAnimation();boundaryTransition=true;boundaryOffset=0f
        reportContentLoading(true)
        fadingBoundaryOut=canFade()
        if(fadingBoundaryOut){
            val remaining=(1f-abs(pendingOffset)/height.coerceAtLeast(1)).coerceIn(0f,1f)
            fadeContent(0f,(60*remaining).toLong().coerceAtLeast(1)){
                fadingBoundaryOut=false
                val ready=pendingPresentation;pendingPresentation=null
                if(ready!=null)present(ready.frame,ready.selection,ready.previous,ready.next,ready.completion)
            }
        }else contentOpacity=0f
        // Start native work now, including when an asynchronous reply is very fast.
        onTurn?.invoke(forward);reportMotion();invalidate()
    }
    private fun canFade()=isShown&&hasWindowFocus()&&ValueAnimator.areAnimatorsEnabled()
    private fun cancelContentFade(){
        val old=contentFade;contentFade=null;old?.cancel()
        contentOpacity=1f;fadingBoundaryOut=false;pendingPresentation=null
    }
    private fun finishContentTransition(){
        // Finishing an outgoing fade may publish its buffered frame and start
        // the incoming fade. Settle both before a new gesture or lifecycle edge.
        repeat(2){contentFade?.end()}
    }
    private fun fadeContent(target:Float,duration:Long,finished:()->Unit={}){
        val old=contentFade;contentFade=null;old?.cancel()
        if(!canFade()){contentOpacity=target;finished();reportMotion();invalidate();return}
        val animator=ValueAnimator.ofFloat(contentOpacity,target)
        contentFade=animator;animator.duration=duration
        animator.interpolator=android.view.animation.PathInterpolator(.25f,.1f,.25f,1f)
        animator.addUpdateListener {contentOpacity=it.animatedValue as Float;invalidate()}
        animator.addListener(object:AnimatorListenerAdapter(){override fun onAnimationEnd(animation:Animator){
            if(contentFade!==animator)return
            contentFade=null;contentOpacity=target;finished();reportMotion();invalidate()
        }})
        reportMotion();animator.start()
    }
    private fun cancelAnimation() { val old=motion;motion=null;motionTarget=null;motionClipped=false;motionVelocity=0f;old?.cancel() }
    private fun afterSlop(delta:Float)=Math.signum(delta)*(abs(delta)-touchSlop).coerceAtLeast(0f)
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
        if(motion!=null&&abs(value-displayPosition)>.25)motionClipped=true
        val offset = pendingOffset
        if (offset != lastReportedInlineOffset) {
            lastReportedInlineOffset = offset
            onInlineOffsetChange?.invoke(offset)
        }
        invalidate()
    }
    private fun animateTo(target:Double,velocity:Float=0f) {
        // Native confirmation of a predicted stop must not restart its curve.
        if(motion!=null&&motionTarget?.let {abs(it-target)<.25}==true&&!motionClipped)return
        val incomingVelocity=if(motion!=null)motionVelocity else velocity
        cancelAnimation()
        if(abs(target-displayPosition)<0.25 || !isShown || !hasWindowFocus()) {
            setPosition(target);previousFrame=null;reportMotion();return
        }
        val start=displayPosition
        val animator=ValueAnimator.ofFloat(0f,1f)
        motion=animator
        motionTarget=target
        motionClipped=false
        val trajectory=focusSettle((target-start).toFloat(),incomingVelocity)
        animator.duration=((trajectory.durationNanos+999_999)/1_000_000).coerceAtLeast(1)
        animator.interpolator=android.view.animation.LinearInterpolator()
        animator.addUpdateListener {
            val time=(it.currentPlayTime*1_000_000).coerceAtMost(trajectory.durationNanos)
            motionVelocity=trajectory.getVelocityFromNanos(time)
            setPosition(start+trajectory.getValueFromNanos(time))
        }
        animator.addListener(object:AnimatorListenerAdapter(){override fun onAnimationEnd(animation:Animator){
            if(motion!==animator)return
            motion=null;motionTarget=null
            if(!navigationPending)previousFrame=null
            reportMotion()
        }})
        reportMotion();animator.start()
    }
    private fun stepFocus(forward:Boolean,velocity:Float=0f) {
        val current=frame ?: return
        if(!current.info.isScroll){onTurn?.invoke(forward);return}
        if(navigationPending||!isEnabled)return
        val target=if(forward)current.info.nextFocus else current.info.previousFocus
        navigationPending=true
        if(target==null){
            // At a semantic unit edge, release commits a separate unit turn.
            // No adjacent-unit text is included in the retained drag strip.
            animateBoundaryTurn(forward);return
        }
        focusPreview=current.info.scrollDistance to target.bounds
        pendingFocusTarget=current.info.scrollDistance+target.delta
        onTurn?.invoke(forward)
        animateTo(current.info.scrollDistance+target.delta,velocity)
        reportMotion()
    }
    private fun finishDrag(velocity:Float,cancelled:Boolean) {
        if(!draggingFocus)return
        draggingFocus=false
        val current=frame?.info
        val destination=if(cancelled)null else focusDragDestination(dragDistance,velocity,current?.previousFocus?.delta,current?.nextFocus?.delta)
        if(deferredFocusGesture&&navigationPending){
            queuedFocusRelease=true;queuedFocusTurn=destination;queuedFocusVelocity=velocity
            if(pendingFocusTarget!=null)animateTo(pendingFocusTarget!!,velocity)
        }else if(destination!=null)stepFocus(destination,velocity)
        else animateTo(frame?.info?.scrollDistance ?: displayPosition,velocity)
        reportMotion()
    }
    private fun requestQueuedUnitTurn(forward:Boolean){
        val navigate=onUnitTurn ?: return
        navigationPending=true;boundaryTransition=true
        boundaryOffset=if(forward)-height/density else height/density
        reportContentLoading(true);invalidate();navigate(forward);reportMotion()
    }
    private fun finishUnitDrag(velocity:Float,cancelled:Boolean) {
        if(!draggingUnit)return
        draggingUnit=false
        if(!cancelled&&pageTurnCommit(unitDistance,width/density,velocity)) {
            if(navigationPending){
                queuedUnitTurn=unitDistance<0;queuedFocusRelease=false;queuedFocusTurn=null
            }else if(!navigationPending)requestQueuedUnitTurn(unitDistance<0)
        }
        reportMotion()
    }
    private fun pageNeighbour()=turnNeighbour
    private fun dragHorizontal(displacement:Float,delta:Float,unit:Boolean){
        val starting=!draggingPage
        draggingPage=true;pageIsUnit=unit
        if(pageDirection==0){
            pageDirection=if(displacement<0)-1 else 1
            turnNeighbour=if(pageDirection<0)nextPage else previousPage
        }
        val extent=pageDistance+(if(starting)afterSlop(displacement)else delta)/density
        pageDistance=if(pageDirection<0)extent.coerceIn(-width/density,0f)else extent.coerceIn(0f,width/density)
        pageOffset=if(pageNeighbour()!=null)pageDistance else pageTurnResistedOffset(pageDistance,width/density)
        parent?.requestDisallowInterceptTouchEvent(true);invalidate();reportMotion()
    }
    private fun finishPageDrag(velocity:Float,cancelled:Boolean){
        if(!draggingPage)return
        draggingPage=false
        val commit=!cancelled&&pageTurnCommit(pageDistance,width/density,velocity)
        cancelAnimation()
        val start=pageOffset
        val target=if(commit&&(pageNeighbour()!=null||pageIsUnit))pageDirection*width/density else 0f
        if(commit&&pageIsUnit&&pageNeighbour()==null)reportContentLoading(true)
        val forward=pageDirection<0
        val unit=pageIsUnit
        val animator=ValueAnimator.ofFloat(start,target)
        motion=animator
        animator.duration=readerTurnDuration(target-start,width/density)
        animator.interpolator=if(unit){if(commit)android.view.animation.PathInterpolator(.215f,.61f,.355f,1f)else android.view.animation.PathInterpolator(0f,0f,.58f,1f)}else android.animation.TimeInterpolator {t->t*t*(3f-2f*t)}
        animator.addUpdateListener {pageOffset=it.animatedValue as Float;invalidate()}
        animator.addListener(object:AnimatorListenerAdapter(){override fun onAnimationEnd(animation:Animator){
            if(motion!==animator)return
            motion=null
            if(commit){navigationPending=true;if(unit)onUnitTurn?.invoke(forward)else onTurn?.invoke(forward)}
            else {pageOffset=0f;pageDirection=0;turnNeighbour=null;invalidate()}
            reportMotion()
        }})
        reportMotion();animator.start()
    }
    private val gestures=GestureDetector(context,object:GestureDetector.SimpleOnGestureListener(){
        override fun onDown(e:MotionEvent)=true
        override fun onSingleTapUp(e:MotionEvent):Boolean {
            performClick()
            val current = frame
            if (current != null && inlineGaps.isEmpty()) {
                val badges = current.info.focusChatBadges
                if (badges.isNotEmpty()) {
                    val tapX = e.x
                    val tapY = e.y + pendingOffset
                    val pad = 10f * density
                    val hit = badges.firstOrNull { b ->
                        val left = b.rect.x * density
                        val top = (b.rect.y + b.rect.height + 4f) * density
                        val right = left + 42f * density
                        val bottom = top + 24f * density
                        tapX in (left - pad)..(right + pad) && tapY in (top - pad)..(bottom + pad)
                    }
                    if (hit != null) {
                        onFocusBadgeTap?.invoke(hit.key)
                        return true
                    }
                }
            }
            onTap?.invoke(e.x/density,(e.y+pendingOffset)/density);return true
        }
        override fun onScroll(a:MotionEvent?,b:MotionEvent,dx:Float,dy:Float):Boolean {
            val current=frame ?: return false
            if(selecting||a==null)return false
            if(embeddedInScrollContainer)return false
            if(gestureAxis==0)gestureAxis=if(abs(b.x-a.x)>abs(b.y-a.y))1 else 2
            if(!reportedGesture&&(current.info.isScroll||abs(b.x-a.x)>abs(b.y-a.y))){reportedGesture=true;onReadingGesture?.invoke()}
            if(!current.info.isScroll){
                if(current.info.focusCount>0u||gestureAxis!=1)return false
                dragHorizontal(b.x-a.x,-dx,false);return true
            }
            if(gestureAxis==1){
                if(draggingUnit||navigationPending){
                    draggingUnit=true;unitDistance=(b.x-a.x)/density
                    parent?.requestDisallowInterceptTouchEvent(true);reportMotion()
                }else dragHorizontal(b.x-a.x,-dx,true)
                return true
            }
            if(gestureAxis!=2)return false
            val starting=!draggingFocus
            draggingFocus=true
            dragDistance+=(if(starting)afterSlop(a.y-b.y)else dy)/density
            if(deferredFocusGesture&&navigationPending&&pendingFocusTarget==null){reportMotion();return true}
            val low=if(deferredFocusGesture)minOf(dragStart,current.info.scrollDistance)else minOf(dragStart,current.info.scrollDistance+(current.info.previousFocus?.delta ?: 0f))
            val high=if(deferredFocusGesture)maxOf(dragStart,pendingFocusTarget ?: dragStart)else maxOf(dragStart,current.info.scrollDistance+(current.info.nextFocus?.delta ?: 0f))
            parent?.requestDisallowInterceptTouchEvent(true)
            setPosition(focusDragPosition(dragStart+dragDistance,low,high,height/density,dragStart));reportMotion();return true
        }
        override fun onFling(a:MotionEvent?,b:MotionEvent,vx:Float,vy:Float):Boolean {
            if(selecting||draggingFocus||draggingPage||a==null)return false
            return false
        }
        override fun onLongPress(e:MotionEvent){
            if(frame==null||!isEnabled||navigationPending||draggingFocus||draggingPage||draggingUnit)return
            selecting=true;anchorX=e.x/density;anchorY=(e.y+pendingOffset)/density
            // Once selection starts, a surrounding note/list must not steal
            // its drag just because the finger began in a margin or line gap.
            parent?.requestDisallowInterceptTouchEvent(true)
            reportMotion()
            onSelect?.invoke(anchorX,anchorY,anchorX,anchorY)
        }
    })
    init {isFocusable=true;ViewCompat.setAccessibilityDelegate(this,accessibility)}
    fun present(next:ReaderFrame?,selection:SelectedText?,previous:ReaderFrame?=null,following:ReaderFrame?=null,turnCompletion:Long=0){
        // A quick close/reopen may be conflated before Compose removes this
        // View. The new reader still needs its viewport even at the same size.
        if(next==null&&width>0&&height>0)onSize?.invoke(width,height,density)
        val old=frame
        // Hold at most one prepared frame until the outgoing content is invisible.
        // Later native/UI confirmations replace it rather than restarting the fade.
        if(fadingBoundaryOut&&(old!==next||turnCompletion!=lastTurnCompletion)){
            pendingPresentation=Presentation(next,selection,previous,following,turnCompletion);return
        }
        var replayFocus:Boolean?=null
        var replayUnit:Boolean?=null
        var replayVelocity=0f
        val completed=turnCompletion!=lastTurnCompletion
        lastTurnCompletion=turnCompletion
        val confirmed=completed&&navigationPending
        if(old!==next||confirmed){
            val continuous=old?.info?.isScroll==true&&next?.info?.isScroll==true&&old.info.scrollContext==next.info.scrollContext
            val pending=navigationPending
            val fadeIn=boundaryTransition&&pageDirection==0&&boundaryOffset==0f
            if(!continuous||confirmed&&pageDirection!=0||pending&&boundaryTransition&&(old!==next||confirmed)){
                val continuing=next?.info?.isScroll==true&&deferredFocusGesture&&(draggingFocus||draggingUnit)
                if(next?.info?.isScroll==true&&pending){
                    replayUnit=queuedUnitTurn
                    if(queuedFocusRelease){replayFocus=queuedFocusTurn;replayVelocity=queuedFocusVelocity}
                }
                stopMotion(continuing);displayPosition=next?.info?.scrollDistance ?: 0.0
                if(continuing)dragStart=displayPosition
                previousFrame=null;frame=next
                if(next!=null&&(fadeIn||old==null&&animateOpening)&&!continuing&&replayFocus==null&&replayUnit==null){
                    contentOpacity=0f;fadeContent(1f,90)
                }
            }else{
                if(old.rasterId!=next.rasterId&&(motion!=null||pending||draggingFocus))previousFrame=old
                frame=next;navigationPending=false;pendingFocusTarget=null;deferredFocusGesture=false
                if(pending)focusPreview=null
                if(queuedUnitTurn!=null){
                    val forward=queuedUnitTurn!!;queuedUnitTurn=null
                    requestQueuedUnitTurn(forward)
                }else if(queuedFocusRelease){
                    val turn=queuedFocusTurn;val velocity=queuedFocusVelocity
                    queuedFocusRelease=false;queuedFocusTurn=null;queuedFocusVelocity=0f
                    if(turn!=null)stepFocus(turn,velocity)else animateTo(next.info.scrollDistance,velocity)
                }else if(!draggingFocus&&!draggingHandle&&pageDirection==0){
                    if(motion==null||pending)animateTo(next.info.scrollDistance)
                }
            }
            accessibility.invalidateRoot();reportMotion()
        }
        previousPage=previous;nextPage=following
        val changed=old!==next||confirmed||selected!=selection
        if (selected != selection) {
            selectionPath.rewind()
            selection?.bounds?.forEach { area ->
                selectionPath.addRect(area.x, area.y, area.x + area.width, area.y + area.height, Path.Direction.CW)
            }
        }
        selected=selection
        if(changed)invalidate()
        if(replayUnit!=null)requestQueuedUnitTurn(replayUnit!!)
        else if(replayFocus!=null)stepFocus(replayFocus!!,replayVelocity)
    }
    override fun onSizeChanged(w:Int,h:Int,oldw:Int,oldh:Int){finishContentTransition();stopMotion();onSize?.invoke(w,h,density)}
    fun presentReference(bounds:org.tortotech.torto.core.Rect?,color:UInt){
        if(referenceIndicator!=bounds||referencePaint.color!=color.toInt()){
            referenceIndicator=bounds;referencePaint.color=color.toInt();invalidate()
        }
    }
    fun presentInlineGaps(gaps: List<Pair<Float, Float>>, shiftYPx: Float) {
        val sorted = gaps.filter { it.first >= 0f && it.second > 0f }.sortedBy { it.first }
        if (sorted.isEmpty()) lastReportedInlineOffset = Float.NaN
        if (inlineGaps != sorted || inlineShiftY != shiftYPx) {
            inlineGaps = sorted
            inlineShiftY = shiftYPx
            invalidate()
        }
    }
    fun dragFocusExternal(deltaYDp: Float) {
        val current = frame ?: return
        if (!current.info.isScroll || !isEnabled || navigationPending) return
        if (!draggingFocus) {
            cancelAnimation()
            draggingFocus = true
            dragStart = displayPosition
            dragDistance = 0f
            deferredFocusGesture = false
            onReadingGesture?.invoke()
        }
        dragDistance += deltaYDp
        val low = minOf(dragStart, current.info.scrollDistance + (current.info.previousFocus?.delta ?: 0f))
        val high = maxOf(dragStart, current.info.scrollDistance + (current.info.nextFocus?.delta ?: 0f))
        setPosition(focusDragPosition(dragStart + dragDistance, low, high, height / density, dragStart))
        reportMotion()
    }
    fun finishFocusExternal(velocityYDp: Float = 0f) {
        if (!draggingFocus) return
        finishDrag(velocityYDp, false)
        reportMotion()
    }
    private fun drawFocusBadges(canvas: Canvas, current: ReaderFrame) {
        // Rendered inline in the Split-Draw slot below the focused paragraph by Compose.
    }
    private fun drawStaticLayer(canvas: Canvas, current: ReaderFrame) {
        if (current.info.isScroll) {
            canvas.drawColor((current.info.focusFadeColor.toInt() and 0xFFFFFF) or 0xFF000000.toInt())
            previousFrame?.let { drawRaster(canvas, it) }
            drawRaster(canvas, current)
        } else {
            canvas.drawBitmap(current.bitmap, null, Rect(0, 0, width, height), paint)
        }
        canvas.save(); canvas.translate(0f, -pendingOffset)
        drawMergedRects(canvas, current.info.marks, savedHighlight)
        val focusShift = focusPreview?.let { (it.first - current.info.scrollDistance).toFloat() } ?: 0f
        (focusPreview?.second ?: current.info.focusBounds).takeIf { it.isNotEmpty() }?.let { bounds ->
            if (current.info.focusHighlight) {
                drawMergedRects(canvas, bounds, highlight, padX = 4f, padY = 2f, shiftY = focusShift)
            } else {
                focusShade.color = current.info.focusFadeColor.toInt(); canvas.save()
                bounds.forEach { r ->
                    canvas.clipOutRect(
                        ((r.x - 8) * density).coerceAtLeast(0f),
                        (r.y + focusShift - 2) * density,
                        ((r.x + r.width + 8) * density).coerceAtMost(width.toFloat()),
                        (r.y + focusShift + r.height + 2) * density,
                    )
                }
                val totalGap = inlineGaps.sumOf { it.second.toDouble() }.toFloat()
                canvas.drawRect(0f, pendingOffset - inlineShiftY, width.toFloat(), height + pendingOffset + inlineShiftY + totalGap, focusShade)
                canvas.restore()
            }
        }
        if (!selectionPath.isEmpty) {
            val layer = canvas.save()
            canvas.scale(density, density)
            canvas.drawPath(selectionPath, highlight)
            canvas.restoreToCount(layer)
        }
        referenceIndicator?.let { r ->
            canvas.drawRoundRect(r.x * density, r.y * density, (r.x + r.width) * density, (r.y + r.height) * density, density, density, referencePaint)
        }
        drawFocusBadges(canvas, current)
        canvas.restore()
    }
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
        if(contentOpacity>=1f){drawContent(canvas);return}
        // A stable paper colour sits below the entire content plane, so text,
        // focus masks and marks fade together without fading the reader chrome.
        frame?.let {canvas.drawColor((it.info.focusFadeColor.toInt() and 0xFFFFFF) or 0xFF000000.toInt())}
        if(contentOpacity<=0f)return
        val layer=canvas.saveLayerAlpha(0f,0f,width.toFloat(),height.toFloat(),(contentOpacity*255).roundToInt())
        drawContent(canvas);canvas.restoreToCount(layer)
    }
    private fun drawContent(canvas:Canvas){
        val current=frame ?: return
        val clipped=canvas.save()
        canvas.clipRect(0,0,width,height)
        if(pageDirection!=0){
            val x=(pageOffset*density).roundToInt().toFloat()
            fun page(f:ReaderFrame,left:Float){
                val layer=canvas.save();canvas.translate(left,0f)
                canvas.clipRect(0,0,width,height)
                canvas.drawColor((f.info.focusFadeColor.toInt() and 0xFFFFFF) or 0xFF000000.toInt())
                val shift=if(f===current)pendingOffset else 0f
                if(f.info.isScroll){
                    val top=(f.info.rasterTop*density-shift).roundToInt().toFloat()
                    canvas.drawBitmap(f.bitmap,null,RectF(0f,top,f.bitmap.width.toFloat(),top+f.bitmap.height),paint)
                    canvas.translate(0f,-shift)
                }else canvas.drawBitmap(f.bitmap,null,Rect(0,0,width,height),paint)
                drawMergedRects(canvas, f.info.marks, savedHighlight)
                if(f.info.focusBounds.isNotEmpty()){
                    if(f.info.focusHighlight)drawMergedRects(canvas, f.info.focusBounds, highlight, padX = 4f, padY = 2f)
                    else {val fade=canvas.save();f.info.focusBounds.forEach {r->canvas.clipOutRect((r.x-8)*density,(r.y-2)*density,(r.x+r.width+8)*density,(r.y+r.height+2)*density)}
                        focusShade.color=f.info.focusFadeColor.toInt();canvas.drawRect(0f,shift,width.toFloat(),height+shift,focusShade);canvas.restoreToCount(fade)}
                }
                canvas.restoreToCount(layer)
            }
            canvas.drawColor((current.info.focusFadeColor.toInt() and 0xFFFFFF) or 0xFF000000.toInt())
            pageNeighbour()?.let {page(it,x-pageDirection*width)}
            page(current,x)
            canvas.restoreToCount(clipped);return
        }
        if(boundaryTransition){
            canvas.drawColor((current.info.focusFadeColor.toInt() and 0xFFFFFF) or 0xFF000000.toInt())
            canvas.translate(0f,boundaryOffset*density)
            canvas.clipRect(0,0,width,height)
        }
        if (inlineGaps.isNotEmpty()) {
            val currentOffset = pendingOffset
            if (currentOffset != lastReportedInlineOffset) {
                lastReportedInlineOffset = currentOffset
                onInlineOffsetChange?.invoke(currentOffset)
            }
            val paperColor = (current.info.focusFadeColor.toInt() and 0xFFFFFF) or 0xFF000000.toInt()
            canvas.drawColor(paperColor)
            val shifted = canvas.save()
            if (inlineShiftY != 0f) canvas.translate(0f, -inlineShiftY)
            var accGap = 0f
            var prevClipTop = -inlineShiftY - 4f
            for ((splitYDp, gapPx) in inlineGaps) {
                val splitPx = (splitYDp * density - currentOffset).roundToInt().toFloat()
                val bandBottom = splitPx + accGap
                if (bandBottom > prevClipTop) {
                    val band = canvas.save()
                    canvas.clipRect(0f, prevClipTop, width.toFloat(), bandBottom)
                    if (accGap != 0f) canvas.translate(0f, accGap)
                    drawStaticLayer(canvas, current)
                    canvas.restoreToCount(band)
                }
                accGap += gapPx
                prevClipTop = splitPx + accGap
            }
            val finalBottom = height.toFloat() + inlineShiftY + 4f
            if (finalBottom > prevClipTop) {
                val band = canvas.save()
                canvas.clipRect(0f, prevClipTop, width.toFloat(), finalBottom)
                if (accGap != 0f) canvas.translate(0f, accGap)
                drawStaticLayer(canvas, current)
                canvas.restoreToCount(band)
            }
            canvas.restoreToCount(shifted)
        } else {
            drawStaticLayer(canvas, current)
        }
        handlePoints().forEach {(x,y)->if(y>=0&&y<=height)canvas.drawCircle(x,y+6*density,7*density,handlePaint)}
        canvas.restoreToCount(clipped)
    }
    override fun onTouchEvent(event:MotionEvent):Boolean {
        if(event.actionMasked==MotionEvent.ACTION_POINTER_DOWN||pinchConsuming){
            fun centroid(e:MotionEvent):Pair<Float,Float> {
                val n=e.pointerCount.coerceAtLeast(1)
                var sx=0f;var sy=0f
                for(i in 0 until n){sx+=e.getX(i);sy+=e.getY(i)}
                return (sx/n) to (sy/n)
            }
            if(!pinchConsuming){
                val control=frame?.info?.controls?.find {it.kind=="sentences"}
                val focusOk=isEnabled&&!navigationPending&&!draggingHandle&&!selecting&&frame?.info?.isScroll==true
                val source=control?.source
                val (cx,cy)=centroid(event)
                multiMaxPointers=event.pointerCount
                multiStartX=cx;multiStartY=cy;multiEndX=cx;multiEndY=cy
                multiStartTime=event.eventTime;multiMoved=false
                val cancel=MotionEvent.obtain(event);cancel.action=MotionEvent.ACTION_CANCEL;gestures.onTouchEvent(cancel);cancel.recycle()
                if(draggingHandle)onHandleEnd?.invoke()
                removeCallbacks(edgeScroll);finishContentTransition();cancelAnimation()
                draggingFocus=false;draggingPage=false;draggingUnit=false;draggingHandle=false;selecting=false
                pageDirection=0;pageOffset=0f;pageDistance=0f;unitDistance=0f;turnNeighbour=null;focusPreview=null
                queuedFocusRelease=false;queuedFocusTurn=null;queuedUnitTurn=null
                setPosition(frame?.info?.scrollDistance ?: 0.0)
                velocity?.recycle();velocity=null
                pinchConsuming=true;focusMultiEligible=focusOk;pinchSource=source
                if(focusMultiEligible)onReadingGesture?.invoke()
                parent?.requestDisallowInterceptTouchEvent(true);reportMotion()
            }
            if(event.pointerCount>multiMaxPointers){
                multiMaxPointers=event.pointerCount
                val (cx,cy)=centroid(event)
                multiStartX=cx;multiStartY=cy;multiEndX=cx;multiEndY=cy
            }
            if(event.pointerCount==multiMaxPointers&&event.actionMasked==MotionEvent.ACTION_MOVE){
                val (cx,cy)=centroid(event)
                multiEndX=cx;multiEndY=cy
                if(hypot(cx-multiStartX,cy-multiStartY)>=touchSlop)multiMoved=true
            }
            if(event.actionMasked==MotionEvent.ACTION_UP||event.actionMasked==MotionEvent.ACTION_CANCEL){
                val dx=multiEndX-multiStartX
                val dy=multiEndY-multiStartY
                val dist=hypot(dx,dy)
                val isUp=event.actionMasked==MotionEvent.ACTION_UP&&focusMultiEligible
                val maxP=multiMaxPointers
                val tapOk=isUp&&maxP==2&&!multiMoved&&(event.eventTime-multiStartTime)<=350L
                val src=pinchSource
                pinchConsuming=false;focusMultiEligible=false;pinchSource=null
                if(tapOk){
                    onFocusChatGesture?.invoke()
                } else if(isUp&&maxP==2&&dist>=touchSlop*3){
                    if(abs(dx)>abs(dy)*1.2f)onFocusHighlightGesture?.invoke(dx>0f)
                    else if(abs(dy)>abs(dx)*1.2f)src?.let {onSentenceGesture?.invoke(dy>0f,it)}
                } else if(isUp&&maxP==3&&abs(dx)>=touchSlop*3&&abs(dx)>abs(dy)*1.2f){
                    onFocusNoteGesture?.invoke(dx>0f)
                }
                parent?.requestDisallowInterceptTouchEvent(false);reportMotion();invalidate()
            }
            return true
        }
        if(event.actionMasked==MotionEvent.ACTION_DOWN){
            finishContentTransition()
            // Complete the stable endpoint before accepting a rapid subsequent drag.
            if((pageDirection!=0||boundaryTransition)&&motion!=null)motion?.end()
            val canDefer=navigationPending&&frame?.info?.isScroll==true
            blockedGesture=(navigationPending&&!canDefer)||!isEnabled
            if(blockedGesture)return true
            deferredFocusGesture=canDefer;queuedFocusRelease=false;queuedFocusTurn=null;queuedFocusVelocity=0f;queuedUnitTurn=null
            cancelAnimation();selecting=false;draggingFocus=false;draggingUnit=false;unitDistance=0f;draggingPage=false
            if(!canDefer||pageDirection==0){pageDirection=0;pageDistance=0f;pageOffset=0f;pageIsUnit=false;turnNeighbour=null}
            reportedGesture=false;dragDistance=0f;dragStart=displayPosition
            gestureAxis=0;releaseVelocityX=0f;releaseVelocityY=0f;lastMoveTime=0L
            velocity?.recycle();velocity=VelocityTracker.obtain()
            val hit=handlePoints().indexOfFirst {(x,y)->hypot(event.x-x,event.y-y-6*density)<26*density}
            if(hit>=0){draggingHandle=true;pointerX=event.x;pointerY=event.y;onHandleStart?.invoke(hit==0);reportMotion();parent?.requestDisallowInterceptTouchEvent(true)}
        }
        if(blockedGesture)return true
        velocity?.addMovement(event)
        if(event.actionMasked==MotionEvent.ACTION_MOVE){
            velocity?.computeCurrentVelocity(1000,maxVelocity)
            releaseVelocityX=velocity?.xVelocity ?: 0f;releaseVelocityY=velocity?.yVelocity ?: 0f;lastMoveTime=event.eventTime
        }
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
            val held=event.eventTime-lastMoveTime>100
            finishDrag(if(held)0f else -releaseVelocityY/density,event.actionMasked==MotionEvent.ACTION_CANCEL)
            finishUnitDrag(if(held)0f else releaseVelocityX/density,event.actionMasked==MotionEvent.ACTION_CANCEL)
            finishPageDrag(if(held)0f else releaseVelocityX/density,event.actionMasked==MotionEvent.ACTION_CANCEL)
            parent?.requestDisallowInterceptTouchEvent(false)
            velocity?.recycle();velocity=null;selecting=false;reportMotion()
        }
        return true
    }
    private fun stopMotion(preserveGesture:Boolean=false){
        val hadHandle=draggingHandle
        cancelContentFade()
        cancelAnimation();navigationPending=false;blockedGesture=false;pageDirection=0;pageDistance=0f;pageOffset=0f;draggingHandle=false;selecting=false
        boundaryTransition=false;boundaryOffset=0f;reportContentLoading(false)
        previousPage=null;nextPage=null;turnNeighbour=null;pageIsUnit=false
        pendingFocusTarget=null;deferredFocusGesture=false;queuedFocusRelease=false;queuedFocusTurn=null;queuedFocusVelocity=0f;queuedUnitTurn=null;motionClipped=false;motionVelocity=0f
        if(!preserveGesture){
            pinchSource=null
            draggingFocus=false;draggingPage=false;draggingUnit=false;unitDistance=0f;gestureAxis=0
            velocity?.recycle();velocity=null
        }
        previousFrame=null;focusPreview=null;displayPosition=frame?.info?.scrollDistance ?: 0.0
        removeCallbacks(edgeScroll);if(hadHandle)onHandleEnd?.invoke();reportMotion()
    }
    fun cancelPendingNavigation(){if(navigationPending||contentLoading){stopMotion();invalidate()}}
    override fun onDetachedFromWindow(){pinchConsuming=false;finishContentTransition();stopMotion();super.onDetachedFromWindow()}
    override fun onWindowFocusChanged(hasWindowFocus:Boolean){super.onWindowFocusChanged(hasWindowFocus);if(!hasWindowFocus){pinchConsuming=false;finishContentTransition();stopMotion()}}
    override fun performClick():Boolean {super.performClick();return true}
    override fun dispatchHoverEvent(event:MotionEvent)=accessibility.dispatchHoverEvent(event)||super.dispatchHoverEvent(event)
    override fun performAccessibilityAction(action:Int,arguments:Bundle?):Boolean {
        if(action==AccessibilityNodeInfoCompat.ACTION_SCROLL_FORWARD || action==AccessibilityNodeInfoCompat.ACTION_SCROLL_BACKWARD){stepFocus(action==AccessibilityNodeInfoCompat.ACTION_SCROLL_FORWARD);sendAccessibilityEvent(AccessibilityEvent.TYPE_VIEW_SCROLLED);return true}
        return super.performAccessibilityAction(action,arguments)
    }
}

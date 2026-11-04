package org.tortotech.torto

import androidx.compose.animation.core.VectorConverter
import androidx.compose.animation.core.TargetBasedAnimation
import androidx.compose.animation.core.spring
import kotlin.math.abs
import kotlin.math.exp
import kotlin.math.sqrt

/** Finite, non-oscillating settling; offsets/velocities stay in logical pixels. */
internal fun focusSettle(distance:Float,velocity:Float):TargetBasedAnimation<Float,androidx.compose.animation.core.AnimationVector1D> {
    val stiffness=900f
    // A critical spring stays monotone if its initial velocity cannot carry it
    // past the stop. Preserve slower/opposing velocities rather than restarting.
    val toward=if(distance>=0)velocity else -velocity
    val limited=toward.coerceIn(-abs(distance)*sqrt(stiffness),abs(distance)*sqrt(stiffness))
    return TargetBasedAnimation(spring(dampingRatio=1f,stiffness=stiffness,visibilityThreshold=.25f),Float.VectorConverter,0f,distance,if(distance>=0)limited else -limited)
}

/** Short stops must not rubber-band past several neighbouring paragraphs. */
internal fun focusDragPosition(value:Double,low:Double,high:Double,viewport:Float,start:Double=0.0):Double {
    val span=if(value<low)start-low else high-start
    val edgeLimit=(viewport*.12f).coerceIn(24f,64f).toDouble()
    val limit=if(span>0.01)minOf(edgeLimit,(span*.25).coerceAtLeast(1.0))else edgeLimit
    return when {
        value<low->low-limit*(1-exp((value-low)/limit))
        value>high->high+limit*(1-exp((high-value)/limit))
        else->value
    }
}

/** One gesture chooses one stop; a reversing fling cancels instead of skipping back. */
internal fun focusDragDestination(distance:Float,velocity:Float,previous:Float?,next:Float?):Boolean? {
    val forward=if(abs(distance)>.01f)distance>0 else velocity>0
    val fling=abs(velocity)>=500f
    if(fling&&(velocity>0)!=forward)return null
    val span=if(forward)next else previous
    val threshold=span?.let {(abs(it)*.5f).coerceIn(8f,36f)} ?: 36f
    return if(abs(distance)>=threshold||fling)forward else null
}

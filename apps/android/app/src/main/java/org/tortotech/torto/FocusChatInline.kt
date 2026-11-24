package org.tortotech.torto

import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectVerticalDragGestures
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.input.nestedscroll.NestedScrollConnection
import androidx.compose.ui.input.nestedscroll.NestedScrollSource
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.Velocity
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.launch
import kotlin.math.roundToInt

@Composable
fun FocusChatInline(
    state: ScreenState,
    splitYDp: Float,
    dragOffsetPx: () -> Float,
    cumulativeBeforePx: Float,
    safeTopPx: Float,
    availableBottomPx: Float,
    maxCardHeightPx: Float,
    onHeightChanged: (gapPx: Float, unconstrainedPx: Float) -> Unit,
    onOverscrollDrag: (Float) -> Unit,
    onOverscrollEnd: (Float) -> Unit,
    onCollapse: () -> Unit,
    onDraftChange: (String) -> Unit,
    onSend: (String) -> Unit,
    onCancel: () -> Unit,
    onLink: (String) -> Unit,
) {
    val density = LocalDensity.current
    val scope = rememberCoroutineScope()
    val focusRequester = remember { FocusRequester() }
    val scrollState = rememberScrollState()
    val assets = remember { AssistantAssetCache() }
    val isWorkingForThisKey = state.focusChatWorking && state.focusChatWorkingKey == state.focusChatKey
    val hasMessages = state.focusChatMessages.isNotEmpty() || isWorkingForThisKey
    var overscrolling by remember { mutableStateOf(false) }
    val collapseDrag = remember(state.focusChatKey) { Animatable(0f) }
    var naturalContentHeightPx by remember(state.focusChatKey) { mutableFloatStateOf(0f) }
    var inputRowHeightPx by remember(state.focusChatKey) { mutableFloatStateOf(with(density) { 38.dp.toPx() }) }
    var cardHeightPx by remember(state.focusChatKey) { mutableFloatStateOf(0f) }
    var collapsingOut by remember(state.focusChatKey) { mutableStateOf(false) }
    val collapseThresholdPx = with(density) { 64.dp.toPx() }
    val latestOverscrollDrag by rememberUpdatedState(onOverscrollDrag)
    val latestOverscrollEnd by rememberUpdatedState(onOverscrollEnd)
    var dragStartShiftPx by remember(state.focusChatKey) { mutableFloatStateOf(0f) }
    var dragStartGapPx by remember(state.focusChatKey) { mutableFloatStateOf(0f) }
    val splitPx = with(density) { splitYDp.dp.toPx() }
    val docSplitPx = splitPx + cumulativeBeforePx
    val maxShiftPx = (docSplitPx - safeTopPx).coerceAtLeast(0f)
    // Exact non-message chrome height: outer Box vertical padding (16dp) + card padding (18+6dp) + handle (14dp) + 2 spacings (16dp) = 70dp
    val chromePx = with(density) { (if (hasMessages) 70.dp else 28.dp).toPx() } + inputRowHeightPx
    val maxBodyPx = (maxCardHeightPx - chromePx).coerceAtLeast(with(density) { 120.dp.toPx() })
    val maxBodyHeightDp = maxBodyPx / density.density
    val unconstrainedCardPx = if (hasMessages && naturalContentHeightPx > 0f) {
        chromePx + naturalContentHeightPx
    } else {
        cardHeightPx
    }
    val instantaneousCardPx = if (unconstrainedCardPx > 0f) unconstrainedCardPx.coerceAtMost(maxCardHeightPx) else cardHeightPx

    fun captureDragStartIfNeeded() {
        if (collapseDrag.value <= 0.5f && instantaneousCardPx > 0f) {
            dragStartShiftPx = (docSplitPx + instantaneousCardPx - availableBottomPx).coerceIn(0f, maxShiftPx)
            dragStartGapPx = instantaneousCardPx
        }
    }
    val latestCollapse by rememberUpdatedState(onCollapse)
    fun finishCollapseDrag(velocityY: Float = 0f) {
        if (collapsingOut) return
        val shouldCollapse = collapseDrag.value >= collapseThresholdPx || velocityY > 600f
        if (shouldCollapse) {
            collapsingOut = true
            scope.launch {
                val target = maxOf(if (naturalContentHeightPx > 0f) minOf(naturalContentHeightPx, maxBodyPx) else maxBodyPx, collapseDrag.value)
                collapseDrag.animateTo(target, tween(200, easing = FastOutSlowInEasing))
                latestCollapse()
            }
        } else {
            scope.launch {
                collapseDrag.animateTo(0f, tween(180, easing = FastOutSlowInEasing))
            }
        }
    }
    val nestedScrollConnection = remember(density, collapseThresholdPx) {
        object : NestedScrollConnection {
            override fun onPreScroll(available: Offset, source: NestedScrollSource): Offset {
                if (source == NestedScrollSource.UserInput && collapseDrag.value > 0f && available.y < 0f) {
                    val consumedY = maxOf(available.y, -collapseDrag.value)
                    scope.launch { collapseDrag.snapTo((collapseDrag.value + consumedY).coerceAtLeast(0f)) }
                    return Offset(0f, consumedY)
                }
                return Offset.Zero
            }
            override fun onPostScroll(consumed: Offset, available: Offset, source: NestedScrollSource): Offset {
                if (source == NestedScrollSource.UserInput && available.y != 0f) {
                    if (available.y > 0f) {
                        captureDragStartIfNeeded()
                        scope.launch { collapseDrag.snapTo((collapseDrag.value + available.y).coerceAtLeast(0f)) }
                        return Offset(0f, available.y)
                    } else {
                        overscrolling = true
                        latestOverscrollDrag(-available.y / density.density)
                        return Offset(0f, available.y)
                    }
                }
                return Offset.Zero
            }
            override suspend fun onPreFling(available: Velocity): Velocity {
                if (collapseDrag.value > 0f) {
                    finishCollapseDrag(available.y)
                    return available
                }
                if (overscrolling) {
                    overscrolling = false
                    latestOverscrollEnd(-available.y / density.density)
                    return available
                }
                return Velocity.Zero
            }
            override suspend fun onPostFling(consumed: Velocity, available: Velocity): Velocity {
                if (collapseDrag.value > 0f) {
                    finishCollapseDrag(available.y)
                    return available
                }
                if (overscrolling) {
                    overscrolling = false
                    latestOverscrollEnd(-available.y / density.density)
                    return available
                }
                return Velocity.Zero
            }
        }
    }

    LaunchedEffect(state.focusChatKey) {
        if (state.focusChatKey != null && state.focusChatMessages.isEmpty()) {
            kotlinx.coroutines.delay(80)
            runCatching { focusRequester.requestFocus() }
        }
    }
    LaunchedEffect(state.focusChatMessages.size, state.focusChatContent.length, isWorkingForThisKey) {
        if (hasMessages) {
            scrollState.scrollTo(scrollState.maxValue)
        }
    }

    val containerColor = MaterialTheme.colorScheme.surface
    val borderColor = MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.55f)

    val collapseLimitPx = if (naturalContentHeightPx > 0f) minOf(naturalContentHeightPx, maxBodyPx) else maxBodyPx
    val effectiveCollapsePx = collapseDrag.value.coerceIn(0f, collapseLimitPx)
    val effectiveMaxBodyDp = if (effectiveCollapsePx > 0.5f) {
        ((collapseLimitPx - effectiveCollapsePx).coerceAtLeast(0f)) / density.density
    } else {
        maxBodyHeightDp
    }
    val collapseFade = if (collapsingOut && collapseLimitPx > 0f) (1f - (effectiveCollapsePx / collapseLimitPx).coerceIn(0f, 1f)) else 1f

    val extraSlideDownPx = if (effectiveCollapsePx > 0.5f) (effectiveCollapsePx - dragStartShiftPx).coerceAtLeast(0f) else 0f
    val currentGapPx = if (effectiveCollapsePx > 0.5f && dragStartGapPx > 0f) {
        (dragStartGapPx - minOf(effectiveCollapsePx, dragStartShiftPx)).coerceAtLeast(0f)
    } else {
        instantaneousCardPx
    }
    val currentShiftPx = if (currentGapPx > 0f) {
        (docSplitPx + currentGapPx - availableBottomPx).coerceIn(0f, maxShiftPx)
    } else 0f
    val reportedUnconstrainedPx = if (effectiveCollapsePx > 0.5f) currentGapPx else unconstrainedCardPx

    SideEffect {
        if (currentGapPx > 0f) {
            onHeightChanged(currentGapPx, reportedUnconstrainedPx)
        }
    }

    Box(
        modifier = Modifier
            .fillMaxWidth()
            .offset { IntOffset(0, (docSplitPx - dragOffsetPx() - currentShiftPx + extraSlideDownPx).roundToInt()) }
            .onSizeChanged {
                val h = it.height.toFloat()
                if (effectiveCollapsePx <= 0.5f) {
                    cardHeightPx = h
                    onHeightChanged(h, if (hasMessages && naturalContentHeightPx > 0f) chromePx + naturalContentHeightPx else h)
                }
            }
            .padding(horizontal = 16.dp, vertical = 8.dp)
            .alpha(collapseFade),
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .clip(RoundedCornerShape(14.dp))
                .background(containerColor)
                .border(1.dp, borderColor, RoundedCornerShape(14.dp))
                .clickable(
                    interactionSource = remember { MutableInteractionSource() },
                    indication = null,
                    onClick = {},
                )
                .padding(start = 12.dp, end = 12.dp, top = if (hasMessages) 18.dp else 6.dp, bottom = 6.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            if (hasMessages) {
                Box(
                    modifier = Modifier
                        .fillMaxWidth()
                        .pointerInput(density, collapseThresholdPx) {
                            detectVerticalDragGestures(
                                onDragStart = { captureDragStartIfNeeded() },
                                onVerticalDrag = { change, dragAmount ->
                                    change.consume()
                                    scope.launch { collapseDrag.snapTo((collapseDrag.value + dragAmount).coerceAtLeast(0f)) }
                                },
                                onDragEnd = { finishCollapseDrag(0f) },
                                onDragCancel = { finishCollapseDrag(0f) },
                            )
                        }
                        .padding(bottom = 10.dp),
                    contentAlignment = Alignment.Center,
                ) {
                    Box(
                        modifier = Modifier
                            .size(width = 36.dp, height = 4.dp)
                            .clip(RoundedCornerShape(2.dp))
                            .background(MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.28f)),
                    )
                }
                Box(
                    modifier = Modifier
                        .fillMaxWidth()
                        .heightIn(max = effectiveMaxBodyDp.dp)
                        .nestedScroll(nestedScrollConnection)
                        .verticalScroll(scrollState),
                ) {
                    Column(
                        modifier = Modifier
                            .fillMaxWidth()
                            .onSizeChanged {
                                naturalContentHeightPx = it.height.toFloat()
                            },
                        verticalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        state.focusChatMessages.forEach { msg ->
                            FocusChatBubble(
                                isUser = msg.user,
                                text = msg.content,
                                stopped = msg.stopped,
                                streaming = false,
                                assets = assets,
                                onLink = onLink,
                            )
                        }
                        if (isWorkingForThisKey) {
                            if (state.focusChatQuestion.isNotBlank()) {
                                FocusChatBubble(
                                    isUser = true,
                                    text = state.focusChatQuestion,
                                    stopped = false,
                                    streaming = false,
                                    assets = assets,
                                    onLink = onLink,
                                )
                            }
                            val streamText = state.focusChatContent.ifBlank {
                                state.focusChatActivity.ifBlank {
                                    if (state.focusChatReasoning.isNotBlank()) tr("思考中…", "Thinking…")
                                    else tr("正在回复…", "Replying…")
                                }
                            }
                            FocusChatBubble(
                                isUser = false,
                                text = streamText,
                                stopped = false,
                                streaming = state.focusChatContent.isNotBlank(),
                                assets = assets,
                                onLink = onLink,
                            )
                        }
                    }
                }
            }

            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .onSizeChanged { inputRowHeightPx = it.height.toFloat() },
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Box(
                    modifier = Modifier
                        .weight(1f)
                        .padding(vertical = 3.dp),
                ) {
                    if (state.focusChatDraft.isEmpty()) {
                        Text(
                            text = tr("针对当前段落提问…", "Ask about this paragraph…"),
                            style = MaterialTheme.typography.bodyMedium.copy(fontSize = 14.sp),
                            color = MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.65f),
                        )
                    }
                    BasicTextField(
                        value = state.focusChatDraft,
                        onValueChange = onDraftChange,
                        modifier = Modifier
                            .fillMaxWidth()
                            .focusRequester(focusRequester),
                        textStyle = MaterialTheme.typography.bodyMedium.copy(
                            fontSize = 14.sp,
                            color = MaterialTheme.colorScheme.onSurface,
                        ),
                        minLines = 1,
                        maxLines = 3,
                        cursorBrush = SolidColor(MaterialTheme.colorScheme.primary),
                    )
                }
                val canSend = state.focusChatDraft.isNotBlank() && !state.focusChatWorking
                Surface(
                    onClick = {
                        if (isWorkingForThisKey) onCancel()
                        else if (canSend) onSend(state.focusChatDraft)
                    },
                    enabled = isWorkingForThisKey || canSend,
                    shape = CircleShape,
                    color = if (isWorkingForThisKey) MaterialTheme.colorScheme.errorContainer
                    else if (canSend) MaterialTheme.colorScheme.primary
                    else MaterialTheme.colorScheme.surfaceContainerHighest,
                    modifier = Modifier.size(38.dp),
                ) {
                    Box(contentAlignment = Alignment.Center) {
                        Text(
                            text = if (isWorkingForThisKey) "■" else "↑",
                            style = MaterialTheme.typography.bodyMedium,
                            fontSize = 18.sp,
                            fontWeight = FontWeight.Bold,
                            color = if (isWorkingForThisKey) MaterialTheme.colorScheme.onErrorContainer
                            else if (canSend) MaterialTheme.colorScheme.onPrimary
                            else MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.5f),
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun FocusChatBubble(
    isUser: Boolean,
    text: String,
    stopped: Boolean,
    streaming: Boolean,
    assets: AssistantAssetCache,
    onLink: (String) -> Unit,
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(10.dp))
            .background(
                if (isUser) MaterialTheme.colorScheme.primaryContainer.copy(alpha = 0.45f)
                else MaterialTheme.colorScheme.surface,
            )
            .padding(horizontal = 10.dp, vertical = 7.dp),
    ) {
        if (isUser) {
            Text(
                text = text,
                fontSize = 16.sp,
                lineHeight = 24.sp,
                color = MaterialTheme.colorScheme.onSurface,
            )
        } else {
            val display = text.ifBlank { if (stopped) tr("已停止生成", "Stopped") else "" }
            if (display.isNotEmpty()) {
                AssistantRichText(
                    text = display,
                    cache = assets,
                    streaming = streaming,
                    onLink = onLink,
                )
            }
        }
    }
}

@Composable
fun FocusChatCollapsedBadge(
    splitYDp: Float,
    dragOffsetPx: () -> Float,
    extraOffsetPx: Float = 0f,
    alpha: Float = 1f,
    onHeightChanged: ((Float) -> Unit)? = null,
    onClick: () -> Unit,
) {
    val density = LocalDensity.current
    val splitPx = with(density) { splitYDp.dp.toPx() }
    Box(
        modifier = Modifier
            .fillMaxWidth()
            .offset { IntOffset(0, (splitPx + extraOffsetPx - dragOffsetPx()).roundToInt()) }
            .then(if (onHeightChanged != null) Modifier.onSizeChanged { onHeightChanged(it.height.toFloat()) } else Modifier)
            .padding(horizontal = 16.dp, vertical = 8.dp)
            .alpha(alpha),
        contentAlignment = Alignment.CenterStart,
    ) {
        Surface(
            onClick = onClick,
            modifier = Modifier.size(50.dp),
            shape = RoundedCornerShape(14.dp),
            color = MaterialTheme.colorScheme.surface,
            border = androidx.compose.foundation.BorderStroke(
                1.dp,
                MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.55f),
            ),
        ) {
            Box(contentAlignment = Alignment.Center) {
                AppIcon("focus-chat", size = 28.dp, tint = MaterialTheme.colorScheme.primary)
            }
        }
    }
}

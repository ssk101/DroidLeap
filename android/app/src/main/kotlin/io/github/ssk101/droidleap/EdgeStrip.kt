package io.github.ssk101.droidleap

import android.accessibilityservice.AccessibilityService
import android.graphics.PixelFormat
import android.view.Gravity
import android.view.MotionEvent
import android.view.View
import android.view.WindowManager
import android.view.WindowManager.LayoutParams

/** A screen edge, named as on the wire. */
enum class Edge(val wireName: String, val gravity: Int, val vertical: Boolean) {
    LEFT("left", Gravity.START or Gravity.TOP, true),
    RIGHT("right", Gravity.END or Gravity.TOP, true),
    TOP("top", Gravity.START or Gravity.TOP, false),
    BOTTOM("bottom", Gravity.START or Gravity.BOTTOM, false);

    companion object {
        fun fromWire(name: String): Edge? = entries.find { it.wireName == name }
    }
}

enum class HoverState(val wireName: String) { ENTER("enter"), MOVE("move"), EXIT("exit") }

/**
 * A thin strip along one screen edge, as an accessibility overlay so it sits
 * above the status bar. The mouse pointer hovering it means the pointer has
 * reached that edge. [onHover] gets the position along the edge, 0 to 1.
 */
class EdgeStrip(
    private val service: AccessibilityService,
    private val onHover: (Edge, HoverState, Double) -> Unit,
) {
    private val windowManager = service.getSystemService(WindowManager::class.java)
    private var view: View? = null

    fun show(edge: Edge) {
        hide()
        val params = LayoutParams(
            if (edge.vertical) STRIP_PX else LayoutParams.MATCH_PARENT,
            if (edge.vertical) LayoutParams.MATCH_PARENT else STRIP_PX,
            LayoutParams.TYPE_ACCESSIBILITY_OVERLAY,
            LayoutParams.FLAG_NOT_FOCUSABLE or LayoutParams.FLAG_LAYOUT_IN_SCREEN,
            PixelFormat.TRANSLUCENT,
        ).apply {
            gravity = edge.gravity
            layoutInDisplayCutoutMode = LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_ALWAYS
            fitInsetsTypes = 0
            title = "droidleap ${edge.wireName}"
        }
        view = View(service).apply {
            // Visible while prototyping, so it's clear which edge is armed.
            setBackgroundColor(IDLE_COLOR)
            setOnHoverListener { v, event ->
                val state = when (event.actionMasked) {
                    MotionEvent.ACTION_HOVER_ENTER -> HoverState.ENTER
                    MotionEvent.ACTION_HOVER_MOVE -> HoverState.MOVE
                    MotionEvent.ACTION_HOVER_EXIT -> HoverState.EXIT
                    else -> return@setOnHoverListener false
                }
                v.setBackgroundColor(if (state == HoverState.EXIT) IDLE_COLOR else HOVER_COLOR)
                val (position, length) = if (edge.vertical) event.y to v.height else event.x to v.width
                onHover(edge, state, (position / (length - 1).coerceAtLeast(1)).toDouble().coerceIn(0.0, 1.0))
                true
            }
        }
        windowManager.addView(view, params)
    }

    fun hide() {
        view?.let(windowManager::removeView)
        view = null
    }

    private companion object {
        const val STRIP_PX = 2
        const val IDLE_COLOR = 0x80FF0000.toInt()
        const val HOVER_COLOR = 0xFF00FF00.toInt()
    }
}

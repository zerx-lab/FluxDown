enum ExternalReturnPhase { normal, externalSheet, returningToSource }

class ExternalReturnStateMachine {
  var _phase = ExternalReturnPhase.normal;
  var _flowId = 0;
  var _wasPaused = false;

  ExternalReturnPhase get phase => _phase;
  bool get shouldHideMainUi => _phase != ExternalReturnPhase.normal;

  int beginExternalSheet() {
    _flowId += 1;
    _phase = ExternalReturnPhase.externalSheet;
    return _flowId;
  }

  bool beginReturn(int flowId) {
    if (flowId != _flowId || _phase != ExternalReturnPhase.externalSheet) {
      return false;
    }
    _phase = ExternalReturnPhase.returningToSource;
    _wasPaused = false;
    return true;
  }

  bool returnFailed(int flowId) {
    if (flowId != _flowId || _phase != ExternalReturnPhase.returningToSource) {
      return false;
    }
    _phase = ExternalReturnPhase.normal;
    return true;
  }

  bool onPaused() {
    if (_phase != ExternalReturnPhase.returningToSource) return false;
    _wasPaused = true;
    return true;
  }

  /// 回到前台：结束"返回来源应用"过渡，恢复显示主界面。
  ///
  /// 不再要求先经历过 `paused`（旧实现对双 Activity 共享引擎时跨任务丢失
  /// paused 事件会永远卡在隐藏主界面 → 黑屏）。只要退出到来源应用的过渡已
  /// 发起且当前不是弹窗打开态，下一次 `resumed` 即恢复显示。
  bool onResumed() {
    if (_phase != ExternalReturnPhase.returningToSource) {
      return false;
    }
    _phase = ExternalReturnPhase.normal;
    _wasPaused = false;
    return true;
  }
}

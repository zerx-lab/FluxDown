import 'package:flutter_test/flutter_test.dart';
import 'package:flux_down/src/mobile/services/external_return_state_machine.dart';

void main() {
  test('keeps the host transparent until a later resume', () {
    final state = ExternalReturnStateMachine();
    final flow = state.beginExternalSheet();

    expect(state.shouldHideMainUi, isTrue);
    expect(state.beginReturn(flow), isTrue);
    expect(state.shouldHideMainUi, isTrue);
    expect(state.onPaused(), isTrue);
    expect(state.onResumed(), isTrue);
    expect(state.shouldHideMainUi, isFalse);
  });

  test('does not reveal the host while the external sheet is open', () {
    final state = ExternalReturnStateMachine();
    state.beginExternalSheet();

    expect(state.onResumed(), isFalse);
    expect(state.shouldHideMainUi, isTrue);
  });

  test('only the current external flow can restore the host on failure', () {
    final state = ExternalReturnStateMachine();
    final oldFlow = state.beginExternalSheet();
    state.beginReturn(oldFlow);
    final currentFlow = state.beginExternalSheet();

    expect(state.returnFailed(oldFlow), isFalse);
    expect(state.shouldHideMainUi, isTrue);
    expect(state.beginReturn(currentFlow), isTrue);
    expect(state.returnFailed(currentFlow), isTrue);
    expect(state.shouldHideMainUi, isFalse);
  });
}

// While a button is loading, clicks (including implicit form submission) are
// swallowed so a pending action can never be triggered twice. Focus stays on
// the button because it is not disabled.
export const blockClick = (event: Event): void => event.preventDefault();

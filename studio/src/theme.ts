// The audit desk in the gate's own material: check-security paper, navy ink, ledger green, stamp red.
// Same tokens as the console (crates/mcp/console/index.html), so the two read as one product.

import { studioTheme } from 'ag-studio';

export const ink = '#1d2b4f';
export const soft = '#52607d';
export const paper = '#e7eee9';
export const sheet = '#f3f7f4';
export const rule = '#b9c6c0';
export const red = '#b3262d';
export const green = '#1f5f45';
export const amber = '#c9861d';
export const FONT = '"Libre Franklin", system-ui, sans-serif';

const line = (color: string) => ({ width: 1, color });
const chart = [ink, red, green, amber, '#5b7fa6', '#8a5a8c', '#3f8f8a', '#a0522d', '#6b7a3a', '#7d8a97'];
const palette = Object.fromEntries(
  chart.flatMap((c, i) => [
    [`chartPaletteFills${i + 1}Color`, c],
    [`chartPaletteStrokes${i + 1}Color`, c],
  ]),
);

export const deskTheme = studioTheme.withParams({
  fontFamily: FONT,
  fontSize: 14,
  textColor: ink,
  foregroundColor: ink,
  subtleTextColor: soft,
  backgroundColor: sheet,
  borderColor: rule,
  accentColor: ink,
  iconColor: ink,
  rowHoverColor: '#dfe8e3',

  studioWrapperBorder: false,
  studioWrapperBorderRadius: 0,
  studioWrapperSpacing: 0,
  studioWrapperBackgroundColor: paper,
  studioCanvasBorder: false,
  studioCanvasBorderRadius: 0,
  studioCanvasShadow: false,
  studioCanvasBackgroundColor: paper,
  studioCanvasPadding: 16,
  studioCanvasRowHeight: 18,
  studioCanvasFontFamily: FONT,

  studioWidgetBackgroundColor: sheet,
  studioWidgetBorder: line(rule),
  studioWidgetBorderRadius: 2,
  studioWidgetPadding: 10,
  studioWidgetTitleFontFamily: FONT,
  studioWidgetTitleFontSize: 14,
  studioWidgetTitleFontWeight: 600,
  studioWidgetTitleTextColor: ink,
  studioWidgetSubtitleFontFamily: FONT,
  studioWidgetSubtitleFontSize: 12,
  studioWidgetSubtitleTextColor: soft,
  studioWidgetCaptionFontFamily: FONT,
  studioWidgetCaptionFontSize: 12,
  studioWidgetCaptionTextColor: soft,
  studioWidgetResizeBorderColor: ink,

  studioPanelContainerBackgroundColor: sheet,
  studioPanelContainerBorder: line(rule),
  studioPanelContainerBorderRadius: 0,
  studioPanelDividerColor: rule,
  studioPanelDividerActiveColor: ink,
  studioPanelSectionBorderColor: rule,
  studioPanelHeaderFontFamily: FONT,
  studioPanelHeaderFontWeight: 600,
  studioToggleButtonActiveBackgroundColor: '#dfe8e3',
  studioToggleButtonActiveBorderColor: ink,
  studioToggleButtonActiveColor: ink,

  gridFontFamily: FONT,
  gridFontSize: 13,
  gridCellTextColor: ink,
  gridHeaderBackgroundColor: paper,
  gridHeaderTextColor: soft,
  gridHeaderFontWeight: 600,
  gridHeaderFontSize: 12,
  gridRowHeight: 34,
  gridRowBorder: line('#d5ded9'),
  gridHeaderRowBorder: line(rule),
  gridWrapperBorder: false,

  chartFontFamily: FONT,
  chartTextColor: ink,
  chartSubtleTextColor: soft,
  chartAxisLineColor: rule,
  chartGridLineColor: '#d5ded9',
  ...palette,
} as any);

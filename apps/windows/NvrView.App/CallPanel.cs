// The alert insets over the stage (docs/UX.md "Alert insets", "Doorbell calls"): one card per
// inset of the window's board (StageView.Insets.cs: a doorbell's call, a conversation, a smoke or
// CO alarm, a person at an entrance, the view a swap put away), in a grid from a corner (Settings,
// else opposite the corner indicators), newest nearest it, the rest behind a "+N more" chip. A
// card is a header (what, which camera, when; a close control that dismisses it, and ends a
// call), the camera's live picture (a click swaps it with the stage), and for a call the call's
// controls: the live captions, an Unmute control over the picture when the
// call opened silent, Talk (hold the button, or turn on "Keep talking"), a text box whose reply is
// spoken through the camera with the system voice. When the camera fills the stage (its tile is
// focused, or is the only one) there is no card: the same controls show along the bottom of the
// stage instead, without a picture of their own. A call popped out into a call window
// (CallWindows.cs) has no card in the viewer windows; the window shows the same card in its
// Window mode, the picture filling the middle. "Keep talking" and the unsent reply text belong
// to the call (Call.Latched, Call.Draft), so a card built later shows them.
//
// The picture is not drawn here: the card's middle is a transparent hole, and the window's own
// render thread draws the camera's shared tile into it, last, as one more tile of its grid
// (StageRenderer.Overlays): no second decoder, the same keyframe gating. Each card with a picture
// holds its own lease on the camera (taken before any is given back, released when the card
// goes), so the tile stays open while the card shows it. The state is Viewer.Calls; the cards only
// show it and send the user's choices back.

using System.Globalization;
using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using NvrView.Shell;
using NvrView.Tiles;
using Windows.System;
using Rect = NvrView.Shell.Rect;
using Size = Windows.Foundation.Size;

namespace NvrView.App;

/// <summary>How a call shows in a window.</summary>
internal enum CallCardMode
{
    /// <summary>A card in the corner with the camera's picture.</summary>
    Inset,

    /// <summary>The camera fills the stage: the controls along its bottom, no picture.</summary>
    Stage,

    /// <summary>The call window's content: the picture fills the middle, Pop In and the pin in the header.</summary>
    Window,
}

/// <summary>One call's card.</summary>
internal sealed class CallCard : UserControl
{
    /// <summary>The border around a card with a picture.</summary>
    public const double BorderWidth = 2;

    private static readonly SolidColorBrush Slate = new(Windows.UI.Color.FromArgb(0xEE, 0x1E, 0x22, 0x28));
    private static readonly SolidColorBrush Dim = new(Windows.UI.Color.FromArgb(0xFF, 0xB8, 0xC0, 0xCA));
    private static readonly SolidColorBrush Accent = new(Windows.UI.Color.FromArgb(0xFF, 0x3A, 0x96, 0xDD));
    private static readonly SolidColorBrush UnmuteCover = new(Windows.UI.Color.FromArgb(0x73, 0x00, 0x00, 0x00));

    private readonly Viewer _viewer;
    private readonly Border _frame;
    private readonly Border _header;
    private readonly Border _body;
    private readonly StackPanel _controls;
    private readonly Grid _picture = new();
    private readonly Border _hole = new() { Background = new SolidColorBrush(Colors.Transparent) };
    private readonly TextBlock _pillText = new() { Foreground = new SolidColorBrush(Colors.White), FontSize = 11, FontWeight = Microsoft.UI.Text.FontWeights.Bold, VerticalAlignment = VerticalAlignment.Center };
    private readonly Border _pill = new() { Background = new SolidColorBrush(Windows.UI.Color.FromArgb(0xD9, 0xE0, 0x24, 0x24)), CornerRadius = new CornerRadius(8), Padding = new Thickness(7, 1, 7, 1), Visibility = Visibility.Collapsed, VerticalAlignment = VerticalAlignment.Center };
    private readonly TextBlock _title = new() { FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, Foreground = new SolidColorBrush(Colors.White), TextTrimming = TextTrimming.CharacterEllipsis, VerticalAlignment = VerticalAlignment.Center };
    private readonly Button _close = new() { Content = "✕", Padding = new Thickness(8, 2, 8, 2), MinWidth = 0, MinHeight = 0 };
    private readonly Button _pop = new() { Content = new FontIcon { Glyph = "\uE8A7", FontSize = 14 }, Padding = new Thickness(8, 2, 8, 2), MinWidth = 0, MinHeight = 0 };
    private readonly ToggleButton _pin = new() { Content = new FontIcon { Glyph = "\uE718", FontSize = 14 }, Padding = new Thickness(8, 2, 8, 2), MinWidth = 0, MinHeight = 0, Visibility = Visibility.Collapsed };
    private readonly RowDefinition _pictureRow = new() { Height = GridLength.Auto };
    private readonly Button _unmute = new() { Content = "Unmute", FontSize = 20, HorizontalAlignment = HorizontalAlignment.Stretch, Padding = new Thickness(0, 10, 0, 10) };
    private readonly TextBlock _captions = new() { Foreground = new SolidColorBrush(Colors.White), TextWrapping = TextWrapping.Wrap, MaxLines = 6, IsTextSelectionEnabled = false };
    private readonly TextBlock _captionNote = new() { Foreground = Dim, FontStyle = Windows.UI.Text.FontStyle.Italic, TextWrapping = TextWrapping.Wrap };
    private readonly TextBlock _status = new() { Foreground = Dim, TextWrapping = TextWrapping.Wrap };
    private readonly Button _talk = new() { Content = "Talk (hold)", MinWidth = 110 };
    private readonly ToggleButton _keep = new() { Content = "Keep talking" };
    private readonly TextBox _reply = new() { PlaceholderText = "Type a reply to speak", HorizontalAlignment = HorizontalAlignment.Stretch };
    private readonly Button _send = new() { Content = "Send" };
    private readonly MenuFlyout _menu;
    private bool _holding;
    private bool _unmuteInPicture;
    private bool _updating;
    private CallCardMode _mode = CallCardMode.Stage;

    public CallCard(Viewer viewer, CameraRef camera)
    {
        _viewer = viewer;
        Camera = camera;
        // The picture's tile of this window's grid: the camera's shared tile, drawn into the hole.
        Tile = new StageTile(viewer, NodePath.Root, camera, TileQuality.Auto);
        AutomationProperties.SetName(this, "Call");

        var header = new Grid { ColumnSpacing = 8 };
        // The call recording's "● Recording 0:42" comes first.
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        _pill.Child = _pillText;
        AutomationProperties.SetName(_pill, "Recording");
        header.Children.Add(_pill);
        Grid.SetColumn(_title, 1);
        header.Children.Add(_title);
        Grid.SetColumn(_pop, 2);
        header.Children.Add(_pop);
        Grid.SetColumn(_pin, 3);
        header.Children.Add(_pin);
        Grid.SetColumn(_close, 4);
        header.Children.Add(_close);
        AutomationProperties.SetName(_close, "End the call");
        ToolTipService.SetToolTip(_close, "End the call");
        _close.Click += (_, _) => CloseRequested();
        _pop.Click += (_, _) => TogglePresentation();
        _pin.Click += (_, _) => PinChanged?.Invoke(_pin.IsChecked == true);
        AutomationProperties.SetName(_pin, "Keep the call window on top");
        ToolTipService.SetToolTip(_pin, "Keep the call window on top of other windows");
        // The card's menu (right-click on the header, the picture, or the controls).
        _menu = BuildMenu();
        ContextFlyout = _menu;

        _unmute.Click += (_, _) => _viewer.Calls.Unmute(Camera);
        AutomationProperties.SetName(_unmute, "Unmute the camera");

        // Talk is held: a press starts it, a release (or losing the pointer) ends it unless
        // "Keep talking" is on. The button handles pointer events itself, so listen to handled ones too.
        _talk.AddHandler(PointerPressedEvent, new PointerEventHandler((_, e) =>
        {
            _holding = true;
            _ = _talk.CapturePointer(e.Pointer);
            _viewer.Calls.StartTalking(Camera);
        }), true);
        PointerEventHandler release = (_, _) => ReleaseTalk();
        _talk.AddHandler(PointerReleasedEvent, release, true);
        _talk.AddHandler(PointerCanceledEvent, release, true);
        _talk.AddHandler(PointerCaptureLostEvent, release, true);
        AutomationProperties.SetName(_talk, "Talk: hold to speak through the camera");
        ToolTipService.SetToolTip(_talk, "Hold to talk through the camera's speaker");
        _keep.Checked += (_, _) =>
        {
            if (!_updating)
            {
                _viewer.Calls.SetLatched(Camera, true);
            }
        };
        _keep.Unchecked += (_, _) =>
        {
            if (!_updating)
            {
                _viewer.Calls.SetLatched(Camera, false, holding: _holding);
            }
        };
        ToolTipService.SetToolTip(_keep, "Keep the microphone open without holding Talk");

        _reply.TextChanged += (_, _) =>
        {
            if (!_updating)
            {
                _viewer.Calls.SetDraft(Camera, _reply.Text);
            }
        };
        _reply.KeyDown += (_, e) =>
        {
            if (e.Key == VirtualKey.Enter)
            {
                SendReply();
                e.Handled = true;
            }
        };
        _send.Click += (_, _) => SendReply();
        ToolTipService.SetToolTip(_send, "Speak the typed text through the camera");

        var talkRow = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        talkRow.Children.Add(_talk);
        talkRow.Children.Add(_keep);
        var replyRow = new Grid { ColumnSpacing = 8 };
        replyRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        replyRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        replyRow.Children.Add(_reply);
        Grid.SetColumn(_send, 1);
        replyRow.Children.Add(_send);

        _controls = new StackPanel { Spacing = 8 };
        _controls.Children.Add(_captions);
        _controls.Children.Add(_captionNote);
        _controls.Children.Add(_status);
        _controls.Children.Add(talkRow);
        _controls.Children.Add(replyRow);

        // Header, picture (a transparent hole the grid draws into; Unmute covers it), controls. Only
        // the header and the controls have a background, so the hole shows the swap chain below.
        _header = new Border { Background = Slate, Padding = new Thickness(12, 8, 8, 8), Child = header };
        _picture.Children.Add(_hole);
        _body = new Border { Background = Slate, Padding = new Thickness(12), Child = _controls };
        var rows = new Grid();
        rows.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        rows.RowDefinitions.Add(_pictureRow);
        rows.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        rows.Children.Add(_header);
        Grid.SetRow(_picture, 1);
        rows.Children.Add(_picture);
        Grid.SetRow(_body, 2);
        rows.Children.Add(_body);
        _frame = new Border { CornerRadius = new CornerRadius(8), Child = rows };
        Content = _frame;
        Mode = CallCardMode.Inset;

        // Using the card is using the call (it stays open); the stage underneath is not to see the
        // card's pointer input (selecting, panning, focusing).
        WireDrag(_header, onHeader: true);
        WireDrag(_hole, onHeader: false);
        PointerEntered += (_, _) =>
        {
            _viewer.Calls.Use(Camera);
            SetHover(true);
        };
        PointerExited += (_, _) => SetHover(false);
        PointerCaptureLost += (_, _) => SetHover(false);
        PointerPressed += (_, e) =>
        {
            _viewer.Calls.Use(Camera);
            e.Handled = true;
        };
        PointerReleased += (_, e) => e.Handled = true;
        DoubleTapped += (_, e) => e.Handled = true;
        RightTapped += (_, e) => e.Handled = true;
        PointerWheelChanged += (_, e) => e.Handled = true;
    }

    // ---- moving the column (docs/UX.md "Alert insets", "Moving them") ----
    // A press on the header, or on the picture, that moves past the slop drags the window's whole
    // column; the pointer's place is measured in the stack (which does not move), so the card
    // moving under it does not feed back. The handlers call the same DragPress/DragMove/DragRelease
    // the self-test calls with synthetic points. A short move on the picture is a click: it swaps the
    // inset with the stage (docs/UX.md "Alert insets").

    private bool _pressed;
    private bool _dragged;
    private bool _onHeader;
    private bool _hovering;
    private Windows.Foundation.Point _pressAt;

    /// <summary>The press has moved far enough to be a drag (self-test).</summary>
    public bool Dragging => _pressed && _dragged;

    private void WireDrag(UIElement part, bool onHeader)
    {
        part.PointerPressed += (_, e) =>
        {
            if (_mode == CallCardMode.Inset && Parent is CallStack stack && e.GetCurrentPoint(part).Properties.IsLeftButtonPressed && part.CapturePointer(e.Pointer))
            {
                DragPress(e.GetCurrentPoint(stack).Position, onHeader);
            }
        };
        part.PointerMoved += (_, e) =>
        {
            if (_pressed && Parent is CallStack stack)
            {
                DragMove(e.GetCurrentPoint(stack).Position);
            }
        };
        part.PointerReleased += (_, e) =>
        {
            part.ReleasePointerCapture(e.Pointer);
            DragRelease();
        };
        part.PointerCaptureLost += (_, _) => DragRelease();
    }

    /// <summary>A press at <paramref name="at"/> (stack DIPs) on the header or the picture.</summary>
    internal void DragPress(Windows.Foundation.Point at, bool onHeader)
    {
        _pressAt = at;
        _onHeader = onHeader;
        _dragged = false;
        _pressed = _mode == CallCardMode.Inset && Parent is CallStack stack && stack.BeginDrag();
    }

    /// <summary>The pointer moved to <paramref name="at"/> with the button down.</summary>
    internal void DragMove(Windows.Foundation.Point at)
    {
        if (!_pressed || Parent is not CallStack stack)
        {
            return;
        }
        double dx = at.X - _pressAt.X, dy = at.Y - _pressAt.Y;
        if (!_dragged && !InsetPlacement.IsDrag(dx, dy, _onHeader))
        {
            return;
        }
        _dragged = true;
        stack.DragBy(dx, dy);
        // Moving the column is using its calls (their minute starts again).
        _viewer.Calls.Use(Camera);
    }

    /// <summary>The button went up (or the capture was lost): a drag stores the position.</summary>
    internal void DragRelease()
    {
        if (!_pressed)
        {
            return;
        }
        _pressed = false;
        bool click = !_dragged && !_onHeader;
        if (Parent is CallStack stack)
        {
            stack.EndDrag(_dragged);
            if (click)
            {
                // Releasing after a drag never swaps; a click on the picture does.
                stack.Owner.SwapInset(Camera);
            }
        }
        _dragged = false;
    }

    /// <summary>The menu's Reset Position: the column returns to the Settings corner.</summary>
    internal void ResetPosition() => (Parent as CallStack)?.ResetPosition();

    // Hovering an inset holds it open (the board's hover); in a call window or over the stage there is no inset to hold.
    private void SetHover(bool inside)
    {
        if (_hovering == inside || Parent is not CallStack stack)
        {
            return;
        }
        _hovering = inside;
        stack.Owner.InsetHover(Camera, inside);
    }

    // The close control and the menu's End/Dismiss: an inset goes (and a call ends); over the stage or in a call window, the call ends.
    private void CloseRequested()
    {
        if (Parent is CallStack stack)
        {
            stack.Owner.CloseInset(Camera);
        }
        else
        {
            _viewer.Calls.End(Camera, "closed");
        }
    }

    /// <summary>The board's inset this card shows, null for a card over the stage or in a call window.</summary>
    public InsetItem? Item { get; private set; }

    /// <summary>The card carries a call (its controls, Pop Out): a call's inset, the call over the stage, or the call window's.</summary>
    public bool IsCall => Item is null || Item.Kind.IsCall;

    /// <summary>The call's camera (its channel's first lens).</summary>
    public CameraRef Camera { get; }

    /// <summary>The picture's tile, drawn by the window's renderer into the hole (while the card holds a lease).</summary>
    public StageTile Tile { get; }

    /// <summary>A card with a picture in the corner, or the controls over the stage.</summary>
    public CallCardMode Mode
    {
        get => _mode;
        set
        {
            if (_mode == value)
            {
                return;
            }
            _mode = value;
            bool inset = value != CallCardMode.Stage;
            bool window = value == CallCardMode.Window;
            _pictureRow.Height = window ? new GridLength(1, GridUnitType.Star) : GridLength.Auto;
            _pin.Visibility = window ? Visibility.Visible : Visibility.Collapsed;
            _close.Visibility = window ? Visibility.Collapsed : Visibility.Visible;
            ApplyPresentationButton();
            // The Unmute control covers the picture, or (over the stage) leads the controls.
            if (inset)
            {
                if (!_unmuteInPicture)
                {
                    if (_unmute.Parent is Panel parent)
                    {
                        parent.Children.Remove(_unmute);
                    }
                    _picture.Children.Add(_unmute);
                    _unmuteInPicture = true;
                }
                _unmute.VerticalAlignment = VerticalAlignment.Stretch;
                _unmute.Background = UnmuteCover;
            }
            else
            {
                if (_unmute.Parent is Panel parent)
                {
                    parent.Children.Remove(_unmute);
                }
                _unmuteInPicture = false;
                _unmute.VerticalAlignment = VerticalAlignment.Top;
                _unmute.ClearValue(Control.BackgroundProperty);
                _controls.Children.Insert(0, _unmute);
            }
            _picture.Visibility = inset ? Visibility.Visible : Visibility.Collapsed;
            _frame.BorderBrush = inset && !window ? BorderFor(Item) : null;
            _frame.BorderThickness = new Thickness(inset && !window ? BorderWidth : 0);
            // In the call window the picture is a regular tile with its own menu; the card's menu stays on the call bar.
            ContextFlyout = window ? null : _menu;
            _header.ContextFlyout = window ? _menu : null;
            _frame.CornerRadius = new CornerRadius(window ? 0 : 8);
            _header.CornerRadius = window ? new CornerRadius(0) : new CornerRadius(inset ? 6 : 8, inset ? 6 : 8, 0, 0);
            _body.CornerRadius = window ? new CornerRadius(0) : new CornerRadius(0, 0, inset ? 6 : 8, inset ? 6 : 8);
        }
    }

    private static readonly SolidColorBrush Critical = new(Windows.UI.Color.FromArgb(0xFF, 0xE0, 0x24, 0x24));

    // Red for smoke and CO, a calmer accent for the rest.
    private static SolidColorBrush BorderFor(InsetItem? item) => item?.Kind.IsCritical == true ? Critical : Accent;

    /// <summary>Whether the card got a place on the stage (an inset that does not fit has none; set by the stack).</summary>
    public bool Placed { get; set; }

    /// <summary>Where the stack put the card, in stage DIPs (self-test).</summary>
    public InsetFrame Frame { get; set; }

    /// <summary>The pin toggle was clicked (the call window's header): keep the window on top.</summary>
    public event Action<bool>? PinChanged;

    /// <summary>Shows the pin's state without raising <see cref="PinChanged"/>.</summary>
    public void SetPinned(bool on) => _pin.IsChecked = on;

    /// <summary>Whether the header offers Pop Out (in the insets) or Pop In (in the call window), self-test.</summary>
    public string PopTip => _mode == CallCardMode.Window ? "Pop In Call" : "Pop Out Call";

    /// <summary>The header's Pop Out / Pop In button is shown (self-test).</summary>
    public bool PopShown => _pop.Visibility == Visibility.Visible;

    /// <summary>The Keep talking toggle shows on (self-test).</summary>
    public bool KeepTalkingShown => _keep.IsChecked == true;

    /// <summary>The reply field's text (self-test).</summary>
    public string ReplyText => _reply.Text;

    /// <summary>The reply field has the keyboard focus (so Esc leaves it instead of ending the call).</summary>
    public bool IsEditingReply => _reply.FocusState != FocusState.Unfocused;

    /// <summary>Pop Out from the insets, Pop In from the call window.</summary>
    private void TogglePresentation()
    {
        if (_mode == CallCardMode.Window)
        {
            _viewer.CallWindows?.PopIn(Camera);
        }
        else
        {
            _viewer.CallWindows?.PopOut(Camera);
        }
    }

    private void ApplyPresentationButton()
    {
        string tip = PopTip;
        _pop.Content = new FontIcon { Glyph = _mode == CallCardMode.Window ? "\uE73F" : "\uE8A7", FontSize = 14 };
        ToolTipService.SetToolTip(_pop, tip);
        AutomationProperties.SetName(_pop, tip);
    }

    private MenuFlyout BuildMenu()
    {
        var menu = new MenuFlyout();
        var pop = new MenuFlyoutItem();
        pop.Click += (_, _) => TogglePresentation();
        var end = new MenuFlyoutItem { Text = "End the call" };
        end.Click += (_, _) => CloseRequested();
        var reset = new MenuFlyoutItem { Text = "Reset Position" };
        reset.Click += (_, _) => ResetPosition();
        var grid = new MenuFlyoutSubItem { Text = "Inset Grid (this window)" };
        menu.Items.Add(pop);
        menu.Items.Add(reset);
        menu.Items.Add(grid);
        menu.Items.Add(end);
        menu.Opening += (_, _) =>
        {
            pop.Text = PopTip;
            pop.Visibility = IsCall ? Visibility.Visible : Visibility.Collapsed;
            end.Text = IsCall ? "End the call" : "Dismiss";
            // Only a card in the corner has a column to move, and only an inset has a grid.
            reset.Visibility = _mode == CallCardMode.Inset ? Visibility.Visible : Visibility.Collapsed;
            grid.Visibility = _mode == CallCardMode.Inset && Parent is CallStack ? Visibility.Visible : Visibility.Collapsed;
            if (Parent is CallStack stack)
            {
                stack.Owner.FillGridMenu(grid);
            }
        };
        return menu;
    }

    /// <summary>
    /// The call window's picture: the window's stage in single fixed tile mode takes the place of the hole
    /// (docs/UX.md "A call in its own window"). Unmute stays above it as the veil inside the tile.
    /// </summary>
    public void HostStage(UIElement stage)
    {
        _hole.Visibility = Visibility.Collapsed;
        _picture.Children.Insert(0, stage);
    }

    /// <summary>The picture size the hole was last sized for.</summary>
    public (int Width, int Height) SizedFor { get; private set; }

    /// <summary>Sizes the picture's hole for the card's width and the picture's aspect (the stage's shape for a whole layout in miniature).</summary>
    public void SizePicture(double cardWidth, double stageWidth, double stageHeight)
    {
        SizedFor = (Tile.PictureWidth, Tile.PictureHeight);
        double width = Math.Max(cardWidth - 2 * BorderWidth, 1);
        _hole.Height = Item is { ShowsLayout: true } && stageWidth > 0 && stageHeight > 0
            ? InsetGeometry.PictureHeight(width, (int)Math.Round(stageWidth), (int)Math.Round(stageHeight))
            : InsetGeometry.PictureHeight(width, Tile.PictureWidth, Tile.PictureHeight);
    }

    /// <summary>The hole's rectangle in <paramref name="stage"/>'s DIPs, null while it is not laid out.</summary>
    public Rect? HoleIn(UIElement stage)
    {
        if (_mode == CallCardMode.Stage || !Placed || _hole.ActualWidth < 1 || _hole.ActualHeight < 1)
        {
            return null;
        }
        Windows.Foundation.Rect r = _hole.TransformToVisual(stage).TransformBounds(new Windows.Foundation.Rect(0, 0, _hole.ActualWidth, _hole.ActualHeight));
        return new Rect(r.X, r.Y, r.Width, r.Height);
    }

    /// <summary>Whether the Unmute control shows (self-test).</summary>
    public bool UnmuteShown => _unmute.Visibility == Visibility.Visible;

    /// <summary>The captions as shown (self-test).</summary>
    public string CaptionText => _captions.Text;

    public string StatusText => _status.Text;

    public string TitleText => _title.Text;

    /// <summary>The call bar's "● Recording 0:42" while the call is recorded, else null (self-test).</summary>
    public string? RecordingPillText => _pill.Visibility == Visibility.Visible ? _pillText.Text : null;

    private void ReleaseTalk()
    {
        if (!_holding)
        {
            return;
        }
        _holding = false;
        if (_keep.IsChecked != true)
        {
            _viewer.Calls.StopTalking(Camera);
        }
    }

    private void SendReply()
    {
        string text = _reply.Text;
        if (text.Trim().Length == 0)
        {
            return;
        }
        // The call empties the box once the reply is on its way; if the user declines the
        // talk-back prompt it keeps what was typed.
        _ = _viewer.Calls.SendReplyAsync(text, Camera);
    }

    /// <summary>Shows the call's state (a card over the stage or in a call window).</summary>
    public void Update() => Update(null, "");

    /// <summary>
    /// Shows <paramref name="item"/>, an inset of the window's board, and for a call its state; null for a card
    /// that is not an inset. <paramref name="layoutName"/> names the whole layout a return inset draws.
    /// </summary>
    public void Update(InsetItem? item, string layoutName)
    {
        Item = item;
        bool call = IsCall;
        if (_mode == CallCardMode.Inset)
        {
            _frame.BorderBrush = BorderFor(item);
        }
        _body.Visibility = call ? Visibility.Visible : Visibility.Collapsed;
        _pop.Visibility = call ? Visibility.Visible : Visibility.Collapsed;
        AutomationProperties.SetName(_close, call ? "End the call" : "Dismiss");
        ToolTipService.SetToolTip(_close, call ? "End the call" : item?.IsReturn == true ? "Back to the stage" : "Dismiss (Esc)");
        if (!call && item is not null)
        {
            _updating = true;
            try
            {
                _title.Text = $"{item.Header(_viewer.WallClock())} · {(item.ShowsLayout ? layoutName : _viewer.CameraName(item.Camera))}";
                _pill.Visibility = Visibility.Collapsed;
                _unmute.Visibility = Visibility.Collapsed;
            }
            finally
            {
                _updating = false;
            }
            return;
        }
        if (_viewer.Calls.CallFor(Camera) is not { } callState)
        {
            return;
        }
        UpdateCall(callState);
    }

    private void UpdateCall(Call call)
    {
        _updating = true;
        try
        {
            string kind = call.Origin == CallOrigin.Conversation && call.Presses == 0 ? "Conversation" : "Doorbell";
            string presses = call.Presses > 1 ? $" · {call.Presses} presses" : "";
            _title.Text = $"{kind} · {_viewer.CameraName(Camera)} · {call.StartedAt.ToString("T", CultureInfo.CurrentCulture)}{presses}";
            if (_viewer.Capture.CallRecording(Camera) is { } recording)
            {
                _pillText.Text = NvrView.Shell.RecordingText.CallPill(recording.DurationMs);
                _pill.Visibility = Visibility.Visible;
            }
            else
            {
                _pill.Visibility = Visibility.Collapsed;
            }
            _unmute.Visibility = call.ShowsUnmute ? Visibility.Visible : Visibility.Collapsed;
            (IReadOnlyList<string> lines, string? note) = call.Captions();
            _captions.Text = string.Join("\n", lines);
            _captions.Visibility = lines.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
            _captionNote.Text = note ?? "";
            _captionNote.Visibility = note is null ? Visibility.Collapsed : Visibility.Visible;
            string extra = _viewer.Calls.NoteFor(Camera) is { } n ? "\n" + n : "";
            _status.Text = call.Talk.Text + extra;
            // The toggle is the call's "Keep talking"; it goes off when the core ended the talk.
            if (_keep.IsChecked != call.Latched)
            {
                _keep.IsChecked = call.Latched;
            }
            // The unsent reply belongs to the call: every card shows the same text.
            if (_reply.Text != call.Draft)
            {
                _reply.Text = call.Draft;
            }
        }
        finally
        {
            _updating = false;
        }
    }
}

/// <summary>
/// The insets of one window's board over its stage (see the file comment): a card for each inset
/// drawn, placed in a grid from the inset corner by <see cref="InsetGeometry"/> (the "+N more" chip
/// after the last), the call of a camera that fills the stage along its bottom, and the camera
/// swapped onto the stage drawn over all of it. Covers the whole stage; only the cards and the chip
/// take pointer input.
/// </summary>
internal sealed class CallStack : Panel
{
    /// <summary>The narrowest a card with a call's controls gets (the controls need the room).</summary>
    public const double MinCardWidth = 340;

    private readonly Viewer _viewer;
    private readonly Dictionary<CameraRef, CallCard> _cards = [];
    private readonly Dictionary<(NodePath, CameraRef), StageTile> _minis = [];
    private readonly Button _chip;
    private List<CallCard> _ordered = [];
    private Dictionary<CameraRef, int> _held = [];
    private OverlayTile[] _published = [];
    private StageTile? _takeover;
    private (LayoutNode Tree, double Width, double Height)? _miniKey;
    private IReadOnlyList<LeafFrame> _miniLeaves = [];
    private string _lastSignature = "";
    private int _more;
    private bool _noRoom;
    private bool _closed;

    /// <param name="viewer">The calls and the leases.</param>
    /// <param name="owner">The window's stage: its board, its grid, the camera that fills it.</param>
    public CallStack(Viewer viewer, StageView owner)
    {
        _viewer = viewer;
        Owner = owner;
        Visibility = Visibility.Collapsed;
        _viewer.Leases.TileChanged += OnTileChanged;
        _chip = new Button
        {
            FontSize = 11,
            FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
            Padding = new Thickness(10, 2, 10, 2),
            MinHeight = 0,
            MinWidth = 0,
            CornerRadius = new CornerRadius(4),
            Background = new SolidColorBrush(Windows.UI.Color.FromArgb(0xB3, 0x00, 0x00, 0x00)),
            Foreground = new SolidColorBrush(Colors.White),
            Opacity = 0,
            IsHitTestVisible = false,
        };
        AutomationProperties.SetName(_chip, "More insets");
        _chip.Click += (_, _) => ShowWaitingMenu();
        Children.Add(_chip);
    }

    /// <summary>The window's stage (the board and what the cards ask of it).</summary>
    public StageView Owner { get; }

    private InsetAnchor? _anchor;
    private InsetFrame? _column;
    private InsetFrame? _dragFrom;

    /// <summary>
    /// Where the user put the column (a fraction of the free space), null while it sits in the
    /// Settings corner. Belongs to the window and is saved with it.
    /// </summary>
    public InsetAnchor? Anchor => _anchor;

    /// <summary>The user moved the column or reset it (the window saves its record).</summary>
    public event Action? AnchorChanged;

    /// <summary>The column of cards as last placed (the union of their frames), null with none.</summary>
    public InsetFrame? Column => _column;

    /// <summary>A position restored with the window (not a change to save).</summary>
    public void RestoreAnchor(InsetAnchor? anchor)
    {
        _anchor = anchor;
        InvalidateArrange();
    }

    /// <summary>A drag starts from the column as it is now; false when there is no column.</summary>
    internal bool BeginDrag()
    {
        _dragFrom = _column;
        return _dragFrom is not null;
    }

    /// <summary>The column's top left moved by (<paramref name="dx"/>, <paramref name="dy"/>) from where the drag began; each axis snaps near an edge.</summary>
    internal void DragBy(double dx, double dy)
    {
        if (_dragFrom is not { } c || ActualWidth <= 0 || ActualHeight <= 0)
        {
            return;
        }
        var moved = InsetPlacement.Anchor(c.X + dx, c.Y + dy, c.Width, c.Height, ActualWidth, ActualHeight);
        if (_anchor != moved)
        {
            _anchor = moved;
            InvalidateArrange();
        }
    }

    /// <summary>The drag ended; a real one stores the position.</summary>
    internal void EndDrag(bool moved)
    {
        _dragFrom = null;
        if (moved)
        {
            AnchorChanged?.Invoke();
        }
    }

    /// <summary>Back to the Settings corner.</summary>
    public void ResetPosition()
    {
        if (_anchor is null)
        {
            return;
        }
        _anchor = null;
        InvalidateArrange();
        AnchorChanged?.Invoke();
    }

    /// <summary>The cards, oldest inset first (the one over the stage, if any, last).</summary>
    public IReadOnlyList<CallCard> Cards => [.. Enumerable.Reverse(_ordered).Concat(_cards.Values.Where(c => c.Mode == CallCardMode.Stage))];

    /// <summary>The leases the cards hold, per camera (self-test).</summary>
    public int LeasesHeld => _held.Values.Sum();

    /// <summary>The pictures last handed to the renderer.</summary>
    public IReadOnlyList<OverlayTile> Overlays => _published;

    /// <summary>The camera swapped onto the stage has a picture (and a stream demand) here.</summary>
    public StageTile? TakeoverTile => _takeover;

    /// <summary>The "+N more" chip's text while it shows, else empty (self-test).</summary>
    public string ChipText => _chip.Opacity > 0 ? (_chip.Content as string ?? "") : "";

    /// <summary>The insets that did not fit the window's height or width, after the board's waiting ones (self-test).</summary>
    public bool Truncated => _noRoom;

    /// <summary>The miniature layout's tiles drawn now (self-test).</summary>
    public IReadOnlyList<StageTile> MiniatureTiles => [.. _minis.Values];

    /// <summary>
    /// Brings the cards in step with the board (after it follows the calls) and what fills the
    /// stage: a card per inset drawn, the call that fills the stage over it, the swapped-in camera.
    /// </summary>
    public void Sync()
    {
        if (_closed)
        {
            return;
        }
        Owner.SyncInsetBoard();
        InsetBoard board = Owner.Board;
        CameraRef? over = CallInsets.OverStage(_viewer.Calls.InsetCameras, Owner.FillingCamera);
        bool changed = false;
        var wantedCards = new HashSet<CameraRef>();
        var ordered = new List<CallCard>();
        foreach (InsetItem item in board.Shown)
        {
            CallCard card = CardFor(item.Camera, ref changed);
            if (card.Mode != CallCardMode.Inset)
            {
                card.Mode = CallCardMode.Inset;
                changed = true;
            }
            card.Update(item, Owner.LayoutName);
            ordered.Add(card);
            wantedCards.Add(item.Camera);
        }
        if (over is { } o && _viewer.Calls.CallFor(o) is not null && wantedCards.Add(o))
        {
            CallCard card = CardFor(o, ref changed);
            if (card.Mode != CallCardMode.Stage)
            {
                card.Mode = CallCardMode.Stage;
                changed = true;
            }
            card.Update();
        }
        foreach (CameraRef gone in _cards.Keys.Where(c => !wantedCards.Contains(c)).ToArray())
        {
            _cards[gone].Tile.Shared = null;
            Children.Remove(_cards[gone]);
            _cards.Remove(gone);
            changed = true;
        }
        _ordered = ordered;

        // One lease per card with a picture (a layout in miniature: one per camera of the layout, the
        // swapped-in camera one more): the new ones are taken before any is given back, so a camera
        // kept is never closed in between.
        var wanted = new Dictionary<CameraRef, int>();
        void Want(CameraRef camera) => wanted[camera] = wanted.GetValueOrDefault(camera) + 1;
        bool layoutCard = false;
        foreach (CallCard card in ordered)
        {
            if (card.Item is { ShowsLayout: true })
            {
                layoutCard = true;
                foreach (CameraRef camera in Owner.State.Tree.Cameras)
                {
                    Want(camera);
                }
            }
            else
            {
                Want(card.Camera);
            }
        }
        CameraRef? swapped = board.Swap?.Shown;
        if (swapped is { } sc)
        {
            Want(sc);
        }
        _viewer.Leases.Update(_held, wanted);
        _held = wanted;
        foreach (CallCard card in _cards.Values)
        {
            card.Tile.Shared = card.Mode == CallCardMode.Inset && card.Item is not { ShowsLayout: true } ? _viewer.Leases.Tile(card.Camera) : null;
        }
        SyncTakeover(swapped);
        SyncMinis(layoutCard);
        Owner.UpdateTakeover();
        Visibility = _cards.Count > 0 || swapped is not null ? Visibility.Visible : Visibility.Collapsed;
        string signature = board.Signature() + "|" + Owner.InsetGrid;
        if (changed || signature != _lastSignature)
        {
            _lastSignature = signature;
            InvalidateMeasure();
        }
    }

    private CallCard CardFor(CameraRef camera, ref bool changed)
    {
        if (!_cards.TryGetValue(camera, out CallCard? card))
        {
            card = new CallCard(_viewer, camera);
            _cards[camera] = card;
            Children.Add(card);
            changed = true;
        }
        return card;
    }

    // The swapped-in camera's picture over the whole stage, asking for the main stream (as a focused tile does).
    private void SyncTakeover(CameraRef? swapped)
    {
        if (_takeover is { } old && swapped != old.CameraOrNone)
        {
            _viewer.WithdrawDemand(old);
            old.Shared = null;
            _takeover = null;
        }
        if (swapped is { } camera)
        {
            if (_takeover is null)
            {
                _takeover = new StageTile(_viewer, NodePath.Root, camera, TileQuality.Auto);
                _viewer.SetFocused(_takeover, true);
            }
            _takeover.Shared = _viewer.Leases.Tile(camera);
        }
    }

    // The tiles of a whole layout drawn in miniature: one per leaf of the window's layout, from the cameras' shared tiles.
    private void SyncMinis(bool wanted)
    {
        if (!wanted)
        {
            foreach (StageTile t in _minis.Values)
            {
                t.Shared = null;
            }
            _minis.Clear();
            return;
        }
        var keep = new HashSet<(NodePath, CameraRef)>();
        foreach (LeafInfo leaf in Owner.State.Tree.Leaves)
        {
            if (leaf.Camera is { } camera)
            {
                keep.Add((leaf.Path, camera));
            }
        }
        foreach ((NodePath, CameraRef) gone in _minis.Keys.Where(k => !keep.Contains(k)).ToArray())
        {
            _minis[gone].Shared = null;
            _minis.Remove(gone);
        }
    }

    /// <summary>The window is closing: every card goes and its lease is given back.</summary>
    public void Close()
    {
        if (_closed)
        {
            return;
        }
        _closed = true;
        _viewer.Leases.TileChanged -= OnTileChanged;
        foreach (CallCard card in _cards.Values)
        {
            card.Tile.Shared = null;
        }
        if (_takeover is { } t)
        {
            _viewer.WithdrawDemand(t);
            t.Shared = null;
            _takeover = null;
        }
        foreach (StageTile mini in _minis.Values)
        {
            mini.Shared = null;
        }
        _minis.Clear();
        _cards.Clear();
        _ordered = [];
        Children.Clear();
        _viewer.Leases.Update(_held, new Dictionary<CameraRef, int>());
        _held = [];
        _published = [];
    }

    // The core tile opened or closed (a reconnect, a refused tier): the pictures follow.
    private void OnTileChanged(CameraRef camera, CameraTile? tile)
    {
        if (_held.ContainsKey(camera) && _cards.TryGetValue(camera, out CallCard? card) && card.Mode == CallCardMode.Inset && card.Item is not { ShowsLayout: true })
        {
            card.Tile.Shared = tile;
        }
        if (_takeover is { } t && t.CameraOrNone == camera)
        {
            t.Shared = tile;
        }
        foreach (KeyValuePair<(NodePath, CameraRef), StageTile> mini in _minis)
        {
            if (mini.Key.Item2 == camera)
            {
                mini.Value.Shared = tile;
            }
        }
    }

    /// <summary>
    /// Once per UI frame: where each picture goes, handed to the renderer when it changed: the
    /// swapped-in camera over the whole stage, then each inset's picture (a layout in miniature as
    /// the layout's own tiles scaled into the inset); a picture whose size changed resizes its card.
    /// </summary>
    public void Publish(StageRenderer renderer, UIElement stage)
    {
        List<OverlayTile> now = [];
        if (_takeover is { } takeover && stage is FrameworkElement fe && fe.ActualWidth >= 1 && fe.ActualHeight >= 1)
        {
            now.Add(new OverlayTile(takeover, new Rect(0, 0, fe.ActualWidth, fe.ActualHeight)));
        }
        // Oldest first, so the top of the stack is drawn last.
        foreach (CallCard card in Enumerable.Reverse(_ordered))
        {
            if (card.Mode != CallCardMode.Inset)
            {
                continue;
            }
            if (card.SizedFor != (card.Tile.PictureWidth, card.Tile.PictureHeight))
            {
                InvalidateMeasure();
            }
            if (card.HoleIn(stage) is not { } rect)
            {
                continue;
            }
            if (card.Item is { ShowsLayout: true })
            {
                now.AddRange(Miniature(rect));
            }
            else
            {
                now.Add(new OverlayTile(card.Tile, rect));
            }
        }
        renderer.Covered = _takeover is not null;
        if (!now.SequenceEqual(_published))
        {
            _published = [.. now];
            renderer.Overlays = _published;
        }
    }

    // The window's layout drawn into the inset's hole: the layout's own arrangement of the tiles, their cameras' pictures fitted into the scaled cells.
    private List<OverlayTile> Miniature(Rect hole)
    {
        LayoutNode tree = Owner.State.Tree;
        if (_miniKey is not { } k || !ReferenceEquals(k.Tree, tree) || k.Width != hole.Width || k.Height != hole.Height)
        {
            _miniLeaves = tree.Frames(new Rect(0, 0, hole.Width, hole.Height), 1).Leaves;
            _miniKey = (tree, hole.Width, hole.Height);
        }
        var tiles = new List<OverlayTile>();
        foreach (LeafFrame leaf in _miniLeaves)
        {
            if (leaf.Camera is not { } camera)
            {
                continue;
            }
            if (!_minis.TryGetValue((leaf.Path, camera), out StageTile? mini))
            {
                mini = new StageTile(_viewer, leaf.Path, camera, TileQuality.Auto) { Shared = _viewer.Leases.Tile(camera) };
                _minis[(leaf.Path, camera)] = mini;
            }
            tiles.Add(new OverlayTile(mini, new Rect(hole.X + leaf.Rect.X, hole.Y + leaf.Rect.Y, leaf.Rect.Width, leaf.Rect.Height)));
        }
        return tiles;
    }

    // The "+N more" chip's list: the cameras waiting for room; choosing one brings it in (the lowest inset waits instead).
    private void ShowWaitingMenu()
    {
        var menu = new MenuFlyout();
        foreach (InsetItem item in Owner.Board.Waiting)
        {
            CameraRef camera = item.Camera;
            var entry = new MenuFlyoutItem { Text = $"{_viewer.CameraName(camera)} · {item.Kind.Text}" };
            entry.Click += (_, _) => Owner.BringInInset(camera);
            menu.Items.Add(entry);
        }
        if (menu.Items.Count > 0)
        {
            menu.ShowAt(_chip);
        }
    }

    private static double Finite(double value, double fallback) => double.IsFinite(value) ? value : fallback;

    // The grid for this stage: how many columns fit (a call's controls need a wider card), and how wide each is.
    private (int Columns, double Width) Columns(double stageWidth, bool anyCall)
    {
        double min = anyCall ? MinCardWidth : InsetGeometry.MinWidth;
        int columns = InsetGeometry.FittingColumns(stageWidth, Owner.InsetGrid.Columns, min);
        return (columns, InsetGeometry.Width(stageWidth, columns, min));
    }

    private InsetAnchor EffectiveAnchor => _anchor ?? InsetAnchor.FromCorner(_viewer.Settings.InsetCorner);

    protected override Size MeasureOverride(Size availableSize)
    {
        double w = Finite(availableSize.Width, ActualWidth), h = Finite(availableSize.Height, ActualHeight);
        CallCard[] insets = [.. _ordered];
        (int columns, double width) = Columns(w, insets.Any(c => c.IsCall));
        foreach (CallCard card in insets)
        {
            card.Width = width;
            card.SizePicture(width, w, h);
            card.Measure(new Size(width, double.PositiveInfinity));
        }
        foreach (CallCard card in _cards.Values.Where(c => c.Mode == CallCardMode.Stage))
        {
            card.Width = InsetGeometry.StagePanel(w, h, 0, null).Width;
            card.Measure(new Size(card.Width, double.PositiveInfinity));
        }
        // What does not fit counts in the chip with the waiting ones (which insets fit does not depend on where the column sits).
        InsetGridSetting grid = Owner.InsetGrid;
        int fit = InsetGeometry.Grid([.. insets.Select(c => c.DesiredSize.Height)], width, grid.Rows, columns, InsetAnchor.FromCorner(ChromeCorner.TopLeft), w, h).Count;
        int cut = insets.Length - fit;
        _noRoom = cut > 0;
        _more = Owner.Board.More + cut;
        _chip.Content = _noRoom ? $"+{_more} more (no room)" : $"+{_more} more";
        ToolTipService.SetToolTip(_chip, _noRoom ? "The window is too small to draw the whole inset grid" : "Click to bring one in");
        bool show = _more > 0 && fit > 0;
        _chip.Opacity = show ? 1 : 0;
        _chip.IsHitTestVisible = show;
        _chip.Measure(new Size(double.PositiveInfinity, double.PositiveInfinity));
        return new Size(w, h);
    }

    protected override Size ArrangeOverride(Size finalSize)
    {
        double w = finalSize.Width, h = finalSize.Height;
        CallCard[] insets = [.. _ordered];
        (int columns, double width) = Columns(w, insets.Any(c => c.IsCall));
        InsetGridSetting grid = Owner.InsetGrid;
        InsetAnchor anchor = EffectiveAnchor;
        // Newest first: the newest inset sits nearest the corner.
        IReadOnlyList<InsetFrame> frames = InsetGeometry.Grid([.. insets.Select(c => c.DesiredSize.Height)], width, grid.Rows, columns, anchor, w, h);
        for (int i = 0; i < insets.Length; i++)
        {
            CallCard card = insets[i];
            card.Placed = i < frames.Count;
            // A card that does not fit is kept off the stage (collapsing it would let it fit again).
            InsetFrame f = card.Placed ? frames[i] : new InsetFrame(-10_000, -10_000, width, card.DesiredSize.Height);
            card.Frame = f;
            card.Arrange(new Windows.Foundation.Rect(f.X, f.Y, f.Width, f.Height));
        }
        InsetFrame? column = InsetGeometry.Union(frames);
        _column = column;
        foreach (CallCard card in _cards.Values.Where(c => c.Mode == CallCardMode.Stage))
        {
            InsetFrame f = InsetGeometry.StagePanel(w, h, card.DesiredSize.Height, column);
            card.Placed = true;
            card.Frame = f;
            card.Arrange(new Windows.Foundation.Rect(f.X, f.Y, f.Width, f.Height));
        }
        // The chip sits after the last inset: below it, or above when the column grows upward.
        Size chip = _chip.DesiredSize;
        if (_chip.Opacity > 0 && frames.Count > 0)
        {
            InsetFrame last = frames[^1];
            double y = anchor.GrowsUpward ? last.Y - 6 - chip.Height : last.Bottom + 6;
            double x = Math.Min(Math.Max(last.X, 0), Math.Max(w - chip.Width, 0));
            _chip.Arrange(new Windows.Foundation.Rect(x, Math.Min(Math.Max(y, 2), Math.Max(h - chip.Height - 2, 2)), chip.Width, chip.Height));
        }
        else
        {
            _chip.Arrange(new Windows.Foundation.Rect(-10_000, -10_000, chip.Width, chip.Height));
        }
        return finalSize;
    }
}

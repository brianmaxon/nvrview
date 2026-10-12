// The calls over the stage (docs/UX.md "Doorbell calls", "Alert insets"): one card per call,
// stacked from a corner (Settings, else opposite the corner indicators), newest nearest it. A card
// is a header (what, which camera, when; a close control that ends the call), the camera's live
// picture, and the call's controls: the live captions, an Unmute control over the picture when the
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
        _close.Click += (_, _) => _viewer.Calls.End(Camera, "closed");
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
        PointerEntered += (_, _) => _viewer.Calls.Use(Camera);
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
    // the self-test calls with synthetic points. A short move on the picture is a click (it does
    // nothing here: there is no swap on Windows).

    private bool _pressed;
    private bool _dragged;
    private bool _onHeader;
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
        if (Parent is CallStack stack)
        {
            stack.EndDrag(_dragged);
        }
        _dragged = false;
    }

    /// <summary>The menu's Reset Position: the column returns to the Settings corner.</summary>
    internal void ResetPosition() => (Parent as CallStack)?.ResetPosition();

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
            _frame.BorderBrush = inset && !window ? Accent : null;
            _frame.BorderThickness = new Thickness(inset && !window ? BorderWidth : 0);
            // In the call window the picture is a regular tile with its own menu; the card's menu stays on the call bar.
            ContextFlyout = window ? null : _menu;
            _header.ContextFlyout = window ? _menu : null;
            _frame.CornerRadius = new CornerRadius(window ? 0 : 8);
            _header.CornerRadius = window ? new CornerRadius(0) : new CornerRadius(inset ? 6 : 8, inset ? 6 : 8, 0, 0);
            _body.CornerRadius = window ? new CornerRadius(0) : new CornerRadius(0, 0, inset ? 6 : 8, inset ? 6 : 8);
        }
    }

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
        end.Click += (_, _) => _viewer.Calls.End(Camera, "closed");
        var reset = new MenuFlyoutItem { Text = "Reset Position" };
        reset.Click += (_, _) => ResetPosition();
        menu.Items.Add(pop);
        menu.Items.Add(reset);
        menu.Items.Add(end);
        menu.Opening += (_, _) =>
        {
            pop.Text = PopTip;
            // Only a card in the corner has a column to move.
            reset.Visibility = _mode == CallCardMode.Inset ? Visibility.Visible : Visibility.Collapsed;
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

    /// <summary>Sizes the picture's hole for the card's width and the picture's aspect.</summary>
    public void SizePicture(double cardWidth)
    {
        SizedFor = (Tile.PictureWidth, Tile.PictureHeight);
        _hole.Height = InsetGeometry.PictureHeight(Math.Max(cardWidth - 2 * BorderWidth, 1), Tile.PictureWidth, Tile.PictureHeight);
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

    /// <summary>Shows the call's state.</summary>
    public void Update()
    {
        if (_viewer.Calls.CallFor(Camera) is not { } call)
        {
            return;
        }
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
/// The cards of every open call over one window's stage (see the file comment): placed from the
/// inset corner by <see cref="InsetGeometry"/>, the call of a camera that fills the stage along
/// its bottom. Covers the whole stage; only the cards take pointer input.
/// </summary>
internal sealed class CallStack : Panel
{
    /// <summary>The narrowest a card with a picture gets (its controls need the room).</summary>
    public const double MinCardWidth = 340;

    private readonly Viewer _viewer;
    private readonly Func<CameraRef?> _filling;
    private readonly Dictionary<CameraRef, CallCard> _cards = [];
    private Dictionary<CameraRef, int> _held = [];
    private OverlayTile[] _published = [];
    private bool _closed;

    /// <param name="viewer">The calls and the leases.</param>
    /// <param name="filling">The camera that fills the window's stage now, if one does.</param>
    public CallStack(Viewer viewer, Func<CameraRef?> filling)
    {
        _viewer = viewer;
        _filling = filling;
        Visibility = Visibility.Collapsed;
        _viewer.Leases.TileChanged += OnTileChanged;
    }

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

    /// <summary>The cards, oldest call first.</summary>
    public IReadOnlyList<CallCard> Cards => [.. _viewer.Calls.InsetCameras.Where(_cards.ContainsKey).Select(c => _cards[c])];

    /// <summary>The leases the cards hold, per camera (self-test).</summary>
    public int LeasesHeld => _held.Values.Sum();

    /// <summary>The pictures last handed to the renderer.</summary>
    public IReadOnlyList<OverlayTile> Overlays => _published;

    /// <summary>Brings the cards in step with the open calls and what fills the stage.</summary>
    public void Sync()
    {
        if (_closed)
        {
            return;
        }
        // A call popped out into its own window has no card here (and its picture and lease go).
        IReadOnlyList<CameraRef> cameras = _viewer.Calls.InsetCameras;
        CameraRef? over = CallInsets.OverStage(cameras, _filling());
        bool changed = false;
        foreach (CameraRef gone in _cards.Keys.Where(c => !cameras.Contains(c)).ToArray())
        {
            Children.Remove(_cards[gone]);
            _cards.Remove(gone);
            changed = true;
        }
        foreach (CameraRef camera in cameras)
        {
            if (!_cards.TryGetValue(camera, out CallCard? card))
            {
                card = new CallCard(_viewer, camera);
                _cards[camera] = card;
                Children.Add(card);
                changed = true;
            }
            CallCardMode mode = camera == over ? CallCardMode.Stage : CallCardMode.Inset;
            if (card.Mode != mode)
            {
                card.Mode = mode;
                changed = true;
            }
            card.Update();
        }
        // One lease per card with a picture: the new ones are taken before any is given back, so a
        // camera kept is never closed in between.
        var wanted = _cards.Values.Where(c => c.Mode == CallCardMode.Inset).ToDictionary(c => c.Camera, _ => 1);
        _viewer.Leases.Update(_held, wanted);
        _held = wanted;
        foreach (CallCard card in _cards.Values)
        {
            card.Tile.Shared = card.Mode == CallCardMode.Inset ? _viewer.Leases.Tile(card.Camera) : null;
        }
        Visibility = _cards.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
        if (changed)
        {
            InvalidateMeasure();
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
        _cards.Clear();
        Children.Clear();
        _viewer.Leases.Update(_held, new Dictionary<CameraRef, int>());
        _held = [];
        _published = [];
    }

    // The core tile opened or closed (a reconnect, a refused tier): the card's picture follows.
    private void OnTileChanged(CameraRef camera, CameraTile? tile)
    {
        if (_held.ContainsKey(camera) && _cards.TryGetValue(camera, out CallCard? card))
        {
            card.Tile.Shared = tile;
        }
    }

    /// <summary>
    /// Once per UI frame: where each card's picture goes, handed to the renderer when it changed;
    /// a picture whose size changed resizes its card.
    /// </summary>
    public void Publish(StageRenderer renderer, UIElement stage)
    {
        List<OverlayTile> now = [];
        foreach (CallCard card in _cards.Values)
        {
            if (card.Mode != CallCardMode.Inset)
            {
                continue;
            }
            if (card.SizedFor != (card.Tile.PictureWidth, card.Tile.PictureHeight))
            {
                InvalidateMeasure();
            }
            if (card.HoleIn(stage) is { } rect)
            {
                now.Add(new OverlayTile(card.Tile, rect));
            }
        }
        if (!now.SequenceEqual(_published))
        {
            _published = [.. now];
            renderer.Overlays = _published;
        }
    }

    private static double Finite(double value, double fallback) => double.IsFinite(value) ? value : fallback;

    private double CardWidth(double stageWidth) =>
        Math.Min(Math.Max(InsetGeometry.Width(stageWidth), MinCardWidth), Math.Max(stageWidth - 2 * InsetGeometry.Margin, 1));

    protected override Size MeasureOverride(Size availableSize)
    {
        double w = Finite(availableSize.Width, ActualWidth), h = Finite(availableSize.Height, ActualHeight);
        double width = CardWidth(w);
        foreach (CallCard card in _cards.Values)
        {
            if (card.Mode == CallCardMode.Inset)
            {
                card.Width = width;
                card.SizePicture(width);
            }
            else
            {
                card.Width = InsetGeometry.StagePanel(w, h, 0, null).Width;
            }
            card.Measure(new Size(card.Width, double.PositiveInfinity));
        }
        return new Size(w, h);
    }

    protected override Size ArrangeOverride(Size finalSize)
    {
        double w = finalSize.Width, h = finalSize.Height;
        double width = CardWidth(w);
        // Newest first: the newest call sits nearest the corner.
        CallCard[] insets = [.. _viewer.Calls.InsetCameras.Reverse().Where(_cards.ContainsKey).Select(c => _cards[c]).Where(c => c.Mode == CallCardMode.Inset)];
        IReadOnlyList<InsetFrame> frames = InsetGeometry.Frames(insets.Length, width, [.. insets.Select(c => c.DesiredSize.Height)], _anchor ?? InsetAnchor.FromCorner(_viewer.Settings.InsetCorner), w, h);
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
        return finalSize;
    }
}

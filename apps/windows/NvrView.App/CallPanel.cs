// The calls over the stage (docs/UX.md "Doorbell calls", "Alert insets"): one card per call,
// stacked from a corner (Settings, else opposite the corner indicators), newest nearest it. A card
// is a header (what, which camera, when; a close control that ends the call), the camera's live
// picture, and the call's controls: the live captions, an Unmute control over the picture when the
// call opened silent, Talk (hold the button, or turn on "Keep talking"), a text box whose reply is
// spoken through the camera with the system voice. When the camera fills the stage (its tile is
// focused, or is the only one) there is no card: the same controls show along the bottom of the
// stage instead, without a picture of their own.
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
    private readonly TextBlock _title = new() { FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, Foreground = new SolidColorBrush(Colors.White), TextTrimming = TextTrimming.CharacterEllipsis, VerticalAlignment = VerticalAlignment.Center };
    private readonly Button _close = new() { Content = "✕", Padding = new Thickness(8, 2, 8, 2), MinWidth = 0, MinHeight = 0 };
    private readonly Button _unmute = new() { Content = "Unmute", FontSize = 20, HorizontalAlignment = HorizontalAlignment.Stretch, Padding = new Thickness(0, 10, 0, 10) };
    private readonly TextBlock _captions = new() { Foreground = new SolidColorBrush(Colors.White), TextWrapping = TextWrapping.Wrap, MaxLines = 6, IsTextSelectionEnabled = false };
    private readonly TextBlock _captionNote = new() { Foreground = Dim, FontStyle = Windows.UI.Text.FontStyle.Italic, TextWrapping = TextWrapping.Wrap };
    private readonly TextBlock _status = new() { Foreground = Dim, TextWrapping = TextWrapping.Wrap };
    private readonly Button _talk = new() { Content = "Talk (hold)", MinWidth = 110 };
    private readonly ToggleButton _keep = new() { Content = "Keep talking" };
    private readonly TextBox _reply = new() { PlaceholderText = "Type a reply to speak", HorizontalAlignment = HorizontalAlignment.Stretch };
    private readonly Button _send = new() { Content = "Send" };
    private bool _holding;
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
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        header.Children.Add(_title);
        Grid.SetColumn(_close, 1);
        header.Children.Add(_close);
        AutomationProperties.SetName(_close, "End the call");
        ToolTipService.SetToolTip(_close, "End the call");
        _close.Click += (_, _) => _viewer.Calls.End(Camera, "closed");

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
                _viewer.Calls.StartTalking(Camera);
            }
        };
        _keep.Unchecked += (_, _) =>
        {
            if (!_updating && !_holding)
            {
                _viewer.Calls.StopTalking(Camera);
            }
        };
        ToolTipService.SetToolTip(_keep, "Keep the microphone open without holding Talk");

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
        rows.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        rows.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        rows.Children.Add(_header);
        Grid.SetRow(_picture, 1);
        rows.Children.Add(_picture);
        Grid.SetRow(_body, 2);
        rows.Children.Add(_body);
        _frame = new Border { CornerRadius = new CornerRadius(8), Child = rows };
        Content = _frame;
        Mode = CallCardMode.Inset;

        // The stage underneath is not to see the card's pointer input (selecting, panning, focusing).
        PointerPressed += (_, e) => e.Handled = true;
        PointerReleased += (_, e) => e.Handled = true;
        DoubleTapped += (_, e) => e.Handled = true;
        RightTapped += (_, e) => e.Handled = true;
        PointerWheelChanged += (_, e) => e.Handled = true;
    }

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
            bool inset = value == CallCardMode.Inset;
            // The Unmute control covers the picture, or (over the stage) leads the controls.
            if (_unmute.Parent is Panel parent)
            {
                parent.Children.Remove(_unmute);
            }
            if (inset)
            {
                _unmute.VerticalAlignment = VerticalAlignment.Stretch;
                _unmute.Background = UnmuteCover;
                _picture.Children.Add(_unmute);
            }
            else
            {
                _unmute.VerticalAlignment = VerticalAlignment.Top;
                _unmute.ClearValue(Control.BackgroundProperty);
                _controls.Children.Insert(0, _unmute);
            }
            _picture.Visibility = inset ? Visibility.Visible : Visibility.Collapsed;
            _frame.BorderBrush = inset ? Accent : null;
            _frame.BorderThickness = new Thickness(inset ? BorderWidth : 0);
            _header.CornerRadius = new CornerRadius(inset ? 6 : 8, inset ? 6 : 8, 0, 0);
            _body.CornerRadius = new CornerRadius(0, 0, inset ? 6 : 8, inset ? 6 : 8);
        }
    }

    /// <summary>Whether the card got a place on the stage (an inset that does not fit has none; set by the stack).</summary>
    public bool Placed { get; set; }

    /// <summary>Where the stack put the card, in stage DIPs (self-test).</summary>
    public InsetFrame Frame { get; set; }

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
        if (_mode != CallCardMode.Inset || !Placed || _hole.ActualWidth < 1 || _hole.ActualHeight < 1)
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
        string text = _reply.Text.Trim();
        if (text.Length == 0)
        {
            return;
        }
        // The box is cleared once the reply is on its way; if the user declines the talk-back
        // prompt it keeps what was typed.
        _ = _viewer.Calls.SendReplyAsync(text, Camera, accepted: () =>
        {
            if (_reply.Text.Trim() == text)
            {
                _reply.Text = "";
            }
        });
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
            _unmute.Visibility = call.ShowsUnmute ? Visibility.Visible : Visibility.Collapsed;
            (IReadOnlyList<string> lines, string? note) = call.Captions();
            _captions.Text = string.Join("\n", lines);
            _captions.Visibility = lines.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
            _captionNote.Text = note ?? "";
            _captionNote.Visibility = note is null ? Visibility.Collapsed : Visibility.Visible;
            string extra = _viewer.Calls.NoteFor(Camera) is { } n ? "\n" + n : "";
            _status.Text = call.Talk.Text + extra;
            // The toggle is the user's choice; it goes off when the core ended the talk.
            if (!_viewer.Calls.IsTalkingWithMicrophone(Camera) && _keep.IsChecked == true)
            {
                _keep.IsChecked = false;
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

    /// <summary>The cards, oldest call first.</summary>
    public IReadOnlyList<CallCard> Cards => [.. _viewer.Calls.Cameras.Where(_cards.ContainsKey).Select(c => _cards[c])];

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
        IReadOnlyList<CameraRef> cameras = _viewer.Calls.Cameras;
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
        CallCard[] insets = [.. _viewer.Calls.Cameras.Reverse().Where(_cards.ContainsKey).Select(c => _cards[c]).Where(c => c.Mode == CallCardMode.Inset)];
        IReadOnlyList<InsetFrame> frames = InsetGeometry.Frames(insets.Length, width, [.. insets.Select(c => c.DesiredSize.Height)], _viewer.Settings.InsetCorner, w, h);
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

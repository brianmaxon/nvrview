// The calls over the stage (docs/UX.md "Doorbell calls"): one card per call in the stage's bottom
// right corner, newest last. Each card has the live captions, an Unmute control when the call
// opened silent, Talk (hold the button, or turn on "Keep talking"), a text box whose reply is
// spoken through the camera with the system voice, and a close control that ends the call. The
// state is Viewer.Calls; the cards only show it and send the user's choices back.

using System.Globalization;
using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using NvrView.Tiles;
using Windows.System;

namespace NvrView.App;

/// <summary>One call's card.</summary>
internal sealed class CallCard : UserControl
{
    private static readonly SolidColorBrush Slate = new(Windows.UI.Color.FromArgb(0xEE, 0x1E, 0x22, 0x28));
    private static readonly SolidColorBrush Dim = new(Windows.UI.Color.FromArgb(0xFF, 0xB8, 0xC0, 0xCA));

    private readonly Viewer _viewer;
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

    public CallCard(Viewer viewer, CameraRef camera)
    {
        _viewer = viewer;
        Camera = camera;
        Width = 380;
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

        var body = new StackPanel { Spacing = 8 };
        body.Children.Add(header);
        body.Children.Add(_unmute);
        body.Children.Add(_captions);
        body.Children.Add(_captionNote);
        body.Children.Add(_status);
        body.Children.Add(talkRow);
        body.Children.Add(replyRow);
        Content = new Border
        {
            Background = Slate,
            CornerRadius = new CornerRadius(8),
            Padding = new Thickness(12),
            Child = body,
        };

        // The stage underneath is not to see the card's pointer input (selecting, panning, focusing).
        PointerPressed += (_, e) => e.Handled = true;
        PointerReleased += (_, e) => e.Handled = true;
        DoubleTapped += (_, e) => e.Handled = true;
        RightTapped += (_, e) => e.Handled = true;
        PointerWheelChanged += (_, e) => e.Handled = true;
    }

    /// <summary>The call's camera (its channel's first lens).</summary>
    public CameraRef Camera { get; }

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
        _reply.Text = "";
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

/// <summary>The cards of every open call, in the stage's bottom right corner.</summary>
internal sealed class CallStack : StackPanel
{
    private readonly Viewer _viewer;
    private readonly Dictionary<CameraRef, CallCard> _cards = [];

    public CallStack(Viewer viewer)
    {
        _viewer = viewer;
        Orientation = Orientation.Vertical;
        Spacing = 8;
        HorizontalAlignment = HorizontalAlignment.Right;
        VerticalAlignment = VerticalAlignment.Bottom;
        Margin = new Thickness(0, 0, 16, 16);
        Visibility = Visibility.Collapsed;
    }

    /// <summary>The cards shown, oldest call first.</summary>
    public IReadOnlyList<CallCard> Cards => [.. Children.OfType<CallCard>()];

    /// <summary>Brings the cards in step with the open calls.</summary>
    public void Sync()
    {
        IReadOnlyList<CameraRef> cameras = _viewer.Calls.Cameras;
        foreach (CameraRef gone in _cards.Keys.Where(c => !cameras.Contains(c)).ToArray())
        {
            Children.Remove(_cards[gone]);
            _cards.Remove(gone);
        }
        foreach (CameraRef camera in cameras)
        {
            if (!_cards.TryGetValue(camera, out CallCard? card))
            {
                card = new CallCard(_viewer, camera);
                _cards[camera] = card;
                Children.Add(card);
            }
            card.Update();
        }
        Visibility = _cards.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
    }
}
